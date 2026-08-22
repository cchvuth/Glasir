//! Embedded terminal (bottom panel, ⌃`): the user's login shell on a PTY in the
//! repository folder, emulated by `alacritty_terminal`. A repo tab keeps its
//! sessions while the panel is hidden. Drawing and key mapping: `ui/terminal.rs`.

use alacritty_terminal::event::{Event, EventListener, WindowSize};
use alacritty_terminal::event_loop::{EventLoop, EventLoopSender, Msg};
use alacritty_terminal::sync::FairMutex;
use alacritty_terminal::term::{Config, Term, test::TermSize};
use alacritty_terminal::tty;
use std::borrow::Cow;
use std::path::Path;
use std::sync::{Arc, mpsc};

/// Hands the PTY thread's events to the UI thread and wakes it up.
#[derive(Clone)]
pub struct Listener {
    ctx: egui::Context,
    tx: mpsc::Sender<Event>,
}

impl EventListener for Listener {
    fn send_event(&self, e: Event) {
        if !matches!(e, Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange) {
            let _ = self.tx.send(e);
        }
        self.ctx.request_repaint();
    }
}

pub struct Session {
    pub term: Arc<FairMutex<Term<Listener>>>,
    tx: EventLoopSender,
    events: mpsc::Receiver<Event>,
    /// Set by the shell (OSC 0/2), else empty.
    pub title: String,
    size: WindowSize,
    /// The shell's pid and a duplicate of the PTY master, to ask which process group
    /// owns the terminal (a command is running when it isn't the shell's).
    #[cfg(unix)]
    shell: (i32, std::fs::File),
    /// Windows: the shell's pid (a command is running when the shell has a child process).
    #[cfg(windows)]
    shell: u32,
}

impl Session {
    pub fn spawn(ctx: &egui::Context, dir: &Path) -> std::io::Result<Session> {
        // `$SHELL -l` (not macOS' default `login -f` wrapper) keeps the shell our direct child.
        #[cfg(unix)]
        let shell = Some(tty::Shell::new(std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or("/bin/sh".into()), vec!["-l".into()]));
        #[cfg(not(unix))]
        let shell = None; // PowerShell
        let opts = tty::Options {
            shell,
            working_directory: Some(dir.to_path_buf()),
            env: [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")].map(|(k, v)| (k.into(), v.into())).into(),
            ..Default::default()
        };
        let size = WindowSize { num_lines: 24, num_cols: 80, cell_width: 8, cell_height: 16 };
        let pty = tty::new(&opts, size, 0)?;
        #[cfg(unix)]
        let shell = (pty.child().id() as i32, pty.file().try_clone()?);
        #[cfg(windows)]
        let shell = pty.child_watcher().pid().map_or(0, |p| p.get());
        let (tx, events) = mpsc::channel();
        let l = Listener { ctx: ctx.clone(), tx };
        let term = Arc::new(FairMutex::new(Term::new(Config::default(), &TermSize::new(80, 24), l.clone())));
        let lp = EventLoop::new(term.clone(), l, pty, false, false)?;
        let tx = lp.channel();
        lp.spawn();
        Ok(Session {
            term,
            tx,
            events,
            title: String::new(),
            size,
            #[cfg(any(unix, windows))]
            shell,
        })
    }

    pub fn write(&self, bytes: impl Into<Cow<'static, [u8]>>) {
        let _ = self.tx.send(Msg::Input(bytes.into()));
    }

    /// Fits the terminal to `cols` x `lines` cells of `cell` pixels.
    pub fn resize(&mut self, cols: u16, lines: u16, cell: (u16, u16)) {
        if (cols, lines) == (self.size.num_cols, self.size.num_lines) || cols < 2 || lines < 1 {
            return;
        }
        self.size = WindowSize { num_lines: lines, num_cols: cols, cell_width: cell.0, cell_height: cell.1 };
        self.term.lock().resize(TermSize::new(cols as usize, lines as usize));
        let _ = self.tx.send(Msg::Resize(self.size));
    }

    /// A command (not just the idle shell) is running in the foreground.
    pub fn busy(&self) -> bool {
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: tcgetpgrp only reads the terminal's foreground process group.
            let fg = unsafe { libc::tcgetpgrp(self.shell.1.as_raw_fd()) };
            fg > 0 && fg != self.shell.0
        }
        // ConPTY has no foreground process group: look for a child of the shell instead.
        #[cfg(windows)]
        // SAFETY: a process snapshot, walked with a correctly sized entry and closed here.
        unsafe {
            use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
            use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
            if self.shell == 0 {
                return false;
            }
            let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
            if snap == INVALID_HANDLE_VALUE {
                return false;
            }
            let mut e: PROCESSENTRY32W = std::mem::zeroed();
            e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
            let mut more = Process32FirstW(snap, &mut e) != 0;
            let mut child = false;
            while more && !child {
                child = e.th32ParentProcessID == self.shell && e.th32ProcessID != self.shell;
                more = Process32NextW(snap, &mut e) != 0;
            }
            CloseHandle(snap);
            child
        }
    }

    /// Handles the shell's requests; false once it exited.
    fn poll(&mut self, ctx: &egui::Context) -> bool {
        for e in self.events.try_iter().collect::<Vec<_>>() {
            match e {
                Event::Title(t) => self.title = t,
                Event::ResetTitle => self.title.clear(),
                Event::PtyWrite(s) => self.write(s.into_bytes()),
                Event::ClipboardStore(_, s) => ctx.copy_text(s),
                Event::TextAreaSizeRequest(f) => self.write(f(self.size).into_bytes()),
                Event::ChildExit(_) | Event::Exit => return false,
                _ => {}
            }
        }
        true
    }
}

impl Drop for Session {
    /// Stops the PTY thread, which hangs up the shell.
    fn drop(&mut self) {
        let _ = self.tx.send(Msg::Shutdown);
    }
}

/// A repo tab's terminal panel.
#[derive(Default)]
pub struct Terminals {
    pub open: bool,
    pub tabs: Vec<Session>,
    pub active: usize,
    /// Why the last shell failed to start.
    pub error: Option<String>,
    /// Give the terminal keyboard focus on the next frame.
    pub focus: bool,
    /// The busy shell whose close button became "Stop & close"; cleared whenever the
    /// list changes, so it never lands on another shell.
    pub confirm: Option<usize>,
}

impl Terminals {
    /// Shows / hides the panel, starting a shell the first time.
    pub fn toggle(&mut self, ctx: &egui::Context, dir: &Path) {
        self.open = !self.open;
        self.focus = self.open;
        if self.open && self.tabs.is_empty() {
            self.add(ctx, dir);
        }
    }

    pub fn add(&mut self, ctx: &egui::Context, dir: &Path) {
        match Session::spawn(ctx, dir) {
            Ok(s) => {
                self.tabs.push(s);
                self.confirm = None;
                self.active = self.tabs.len() - 1;
                self.error = None;
                self.focus = true;
            }
            Err(e) => self.error = Some(format!("Couldn't start a shell: {e}")),
        }
    }

    /// Drops exited shells; hides the panel when the last one exits.
    pub fn poll(&mut self, ctx: &egui::Context) {
        let n = self.tabs.len();
        self.tabs.retain_mut(|s| s.poll(ctx));
        if self.tabs.len() < n {
            self.fix_after_removal();
        }
    }

    /// Stops shell `i` (SIGHUP to whatever runs in it).
    pub fn close(&mut self, i: usize) {
        if i < self.tabs.len() {
            self.tabs.remove(i);
            self.fix_after_removal();
        }
    }

    fn fix_after_removal(&mut self) {
        self.active = self.active.min(self.tabs.len().saturating_sub(1));
        self.open &= !self.tabs.is_empty();
        self.confirm = None;
    }

    /// Settings → Terminal is off: idle shells end; one still running a command is kept
    /// (shown or hidden) until it finishes or is stopped.
    pub fn keep_busy(&mut self) {
        let n = self.tabs.len();
        self.tabs.retain(Session::busy);
        if self.tabs.len() < n {
            self.fix_after_removal();
        }
    }

    /// A command is running in one of the sessions.
    pub fn busy(&self) -> bool {
        self.tabs.iter().any(Session::busy)
    }

    /// How many sessions run a command.
    pub fn running(&self) -> usize {
        self.tabs.iter().filter(|s| s.busy()).count()
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::time::{Duration, Instant};

    fn wait(what: &str, mut f: impl FnMut() -> bool) {
        let t = Instant::now();
        while !f() {
            assert!(t.elapsed() < Duration::from_secs(30), "timed out waiting for {what}");
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn screen(s: &Session) -> String {
        let t = s.term.lock();
        let c = t.renderable_content();
        c.display_iter.map(|i| i.cell.c).collect()
    }

    #[test]
    fn shell_runs_in_the_repo_and_reports_busy_commands() {
        let dir = crate::repo::testutil::scratch("terminal");
        let ctx = egui::Context::default();
        let mut ts = Terminals::default();
        ts.toggle(&ctx, &dir);
        assert!(ts.open && ts.tabs.len() == 1, "{:?}", ts.error);
        let s = &ts.tabs[0];
        s.write(&b"basename \"$PWD\"; echo do''ne\r"[..]);
        wait("pwd", || screen(s).contains("done"));
        let name = dir.file_name().unwrap().to_string_lossy().into_owned();
        assert!(screen(s).contains(&name), "{}", screen(s));
        wait("prompt", || !s.busy());
        s.write(&b"sleep 5\r"[..]);
        wait("busy", || s.busy());
        s.write(&b"\x03"[..]);
        wait("idle", || !s.busy());
        ts.tabs[0].write(&b"exit\r"[..]);
        wait("exit", || {
            ts.poll(&ctx);
            ts.tabs.is_empty()
        });
        assert!(!ts.open);
    }

    #[test]
    fn a_pending_stop_never_moves_to_another_shell() {
        let dir = crate::repo::testutil::scratch("terminal-confirm");
        let ctx = egui::Context::default();
        let mut ts = Terminals::default();
        ts.toggle(&ctx, &dir);
        ts.add(&ctx, &dir);
        ts.confirm = Some(1);
        ts.tabs[0].write(&b"exit\r"[..]);
        wait("exit", || {
            ts.poll(&ctx);
            ts.tabs.len() == 1
        });
        assert_eq!(ts.confirm, None, "shell 2 is now index 0; a stale Some(0) would also hit a new shell");
        ts.confirm = Some(0);
        ts.add(&ctx, &dir);
        assert_eq!(ts.confirm, None);
    }

    #[test]
    fn turning_the_terminal_off_ends_idle_shells_and_shows_busy_ones() {
        let dir = crate::repo::testutil::scratch("terminal-off");
        let ctx = egui::Context::default();
        let mut ts = Terminals::default();
        ts.toggle(&ctx, &dir);
        ts.add(&ctx, &dir);
        // Past the login scripts (macOS' path_helper runs in the foreground too).
        for s in &ts.tabs {
            s.write(&b"echo re''ady\r"[..]);
            wait("prompt", || screen(s).contains("ready") && !s.busy());
        }
        ts.tabs[1].write(&b"echo sta''rted; sleep 30\r"[..]);
        wait("busy", || screen(&ts.tabs[1]).contains("started") && ts.tabs[1].busy());
        ts.keep_busy();
        assert!(ts.open && ts.tabs.len() == 1 && ts.active == 0 && ts.tabs[0].busy());
        ts.open = false; // Hide: the command keeps running out of sight
        ts.keep_busy();
        assert!(!ts.open && ts.tabs.len() == 1);
        ts.tabs[0].write(&b"\x03"[..]);
        wait("idle", || !ts.busy());
        ts.keep_busy();
        assert!(!ts.open && ts.tabs.is_empty());
    }
}
