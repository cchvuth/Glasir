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
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

/// Hands the PTY thread's events to the UI thread and wakes it up.
#[derive(Clone)]
pub struct Listener {
    ctx: egui::Context,
    tx: mpsc::Sender<Event>,
    /// The egui pass in which the grid was last drawn.
    drawn: Arc<AtomicU64>,
}

impl EventListener for Listener {
    fn send_event(&self, e: Event) {
        let wakeup = matches!(e, Event::Wakeup);
        if !matches!(e, Event::Wakeup | Event::MouseCursorDirty | Event::CursorBlinkingChange) {
            let _ = self.tx.send(e);
        }
        // Output nobody sees (hidden panel, background tab) only wakes the app once a second.
        if !wakeup || self.ctx.cumulative_pass_nr() <= self.drawn.load(Ordering::Relaxed) + 1 {
            self.ctx.request_repaint();
        } else {
            self.ctx.request_repaint_after(Duration::from_secs(1));
        }
    }
}

pub struct Session {
    pub term: Arc<FairMutex<Term<Listener>>>,
    tx: EventLoopSender,
    events: mpsc::Receiver<Event>,
    /// Set by the shell (OSC 0/2), else empty.
    pub title: String,
    size: WindowSize,
    /// See `Listener::drawn`; set by the grid.
    pub drawn: Arc<AtomicU64>,
    /// Since when `keep_busy` has seen the shell idle.
    idle_since: Option<Instant>,
    /// The shell's pid and a duplicate of the PTY master, to ask which process group
    /// owns the terminal (a command is running when it isn't the shell's).
    #[cfg(unix)]
    shell: (i32, std::fs::File),
    /// Windows: the shell's pid and creation time (a command is running when the shell
    /// has a child process).
    #[cfg(windows)]
    shell: (u32, u64),
    /// Windows: the last `busy()` answer and when it was taken; a process snapshot is
    /// too slow to take every frame.
    #[cfg(windows)]
    seen: std::cell::Cell<Option<(std::time::Instant, bool)>>,
}

impl Session {
    pub fn spawn(ctx: &egui::Context, dir: &Path) -> std::io::Result<Session> {
        // `$SHELL -l` (not macOS' default `login -f` wrapper) keeps the shell our direct child.
        #[cfg(all(unix, not(test)))]
        let shell = Some(tty::Shell::new(std::env::var("SHELL").ok().filter(|s| !s.is_empty()).unwrap_or("/bin/sh".into()), vec!["-l".into()]));
        #[cfg(all(unix, test))]
        let shell = Some(tty::Shell::new("/bin/sh".into(), vec![]));
        #[cfg(not(unix))]
        let shell = None; // PowerShell
        let opts = tty::Options {
            shell,
            working_directory: Some(dir.to_path_buf()),
            env: [("TERM", "xterm-256color"), ("COLORTERM", "truecolor")].map(|(k, v)| (k.into(), v.into())).into(),
            ..Default::default()
        };
        let size = WindowSize { num_lines: 24, num_cols: 80, cell_width: 8, cell_height: 16 };
        let pty = crate::git::spawning(|| tty::new(&opts, size, 0))?;
        #[cfg(unix)]
        let shell = (pty.child().id() as i32, pty.file().try_clone()?);
        #[cfg(windows)]
        let shell = (pty.child_watcher().pid().map_or(0, |p| p.get()), created(pty.child_watcher().raw_handle()));
        let (tx, events) = mpsc::channel();
        let drawn = Arc::new(AtomicU64::new(ctx.cumulative_pass_nr()));
        let l = Listener { ctx: ctx.clone(), tx, drawn: drawn.clone() };
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
            drawn,
            idle_since: None,
            #[cfg(any(unix, windows))]
            shell,
            #[cfg(windows)]
            seen: Default::default(),
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
        #[cfg(windows)]
        {
            let now = std::time::Instant::now();
            match self.seen.get() {
                Some((at, b)) if now - at < std::time::Duration::from_millis(500) => b,
                _ => {
                    let b = has_child(self.shell);
                    self.seen.set(Some((now, b)));
                    b
                }
            }
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

/// A process handle's creation time (0 if unknown).
#[cfg(windows)]
fn created(h: windows_sys::Win32::Foundation::HANDLE) -> u64 {
    use windows_sys::Win32::Foundation::FILETIME;
    let mut t = [FILETIME { dwLowDateTime: 0, dwHighDateTime: 0 }; 4];
    let [c, e, k, u] = &mut t;
    // SAFETY: GetProcessTimes only writes the four FILETIMEs passed in.
    if unsafe { windows_sys::Win32::System::Threading::GetProcessTimes(h, c, e, k, u) } == 0 {
        return 0;
    }
    (t[0].dwHighDateTime as u64) << 32 | t[0].dwLowDateTime as u64
}

/// ConPTY has no foreground process group: look for a child of the shell instead.
/// A process orphaned before the shell started may still name the shell's (reused)
/// pid as its parent, so only processes created after the shell count.
#[cfg(windows)]
fn has_child((shell, born): (u32, u64)) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, INVALID_HANDLE_VALUE};
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, PROCESSENTRY32W, Process32FirstW, Process32NextW, TH32CS_SNAPPROCESS};
    use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION};
    if shell == 0 {
        return false;
    }
    // SAFETY: a process snapshot walked with a correctly sized entry; every handle
    // opened here is closed here.
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return false;
        }
        let mut e: PROCESSENTRY32W = std::mem::zeroed();
        e.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snap, &mut e) != 0;
        let mut child = false;
        while more && !child {
            if e.th32ParentProcessID == shell && e.th32ProcessID != shell {
                let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, e.th32ProcessID);
                if !h.is_null() {
                    child = created(h) >= born;
                    CloseHandle(h);
                }
            }
            more = Process32NextW(snap, &mut e) != 0;
        }
        CloseHandle(snap);
        child
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
        // Between the jobs of `a; b` the shell briefly owns the terminal: idle must last.
        self.tabs.retain_mut(|s| {
            if s.busy() {
                s.idle_since = None;
                return true;
            }
            s.idle_since.get_or_insert_with(Instant::now).elapsed() < Duration::from_secs(1)
        });
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

#[cfg(test)]
mod tests {
    use super::*;

    use crate::repo::testutil::wait_for as wait;

    fn screen(s: &Session) -> String {
        let t = s.term.lock();
        let c = t.renderable_content();
        c.display_iter.map(|i| i.cell.c).collect()
    }

    #[cfg(unix)]
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
        s.write(&b"sleep 60\r"[..]); // ^C ends it: long enough to be seen busy on a loaded machine
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

    #[cfg(unix)]
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

    #[cfg(unix)]
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
        wait("idle shell dropped", || {
            ts.keep_busy();
            ts.tabs.len() == 1
        });
        assert!(ts.open && ts.active == 0 && ts.tabs[0].busy());
        ts.open = false; // Hide: the command keeps running out of sight
        ts.keep_busy();
        assert!(!ts.open && ts.tabs.len() == 1);
        ts.tabs[0].write(&b"\x03"[..]);
        wait("idle", || !ts.busy());
        wait("dropped", || {
            ts.keep_busy();
            ts.tabs.is_empty()
        });
        assert!(!ts.open);
    }

    #[cfg(unix)]
    #[test]
    fn keep_busy_spares_a_multi_command_line() {
        let dir = crate::repo::testutil::scratch("terminal-gap");
        let ctx = egui::Context::default();
        let mut ts = Terminals::default();
        ts.toggle(&ctx, &dir);
        let s = &ts.tabs[0];
        s.write(&b"echo re''ady\r"[..]);
        wait("prompt", || screen(s).contains("ready") && !s.busy());
        s.write(&b"for i in 1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20; do sleep 0.1; done; echo fin''ished\r"[..]);
        // No deadline of its own: on a loaded machine the 20 sleeps take as long as they take.
        wait("finished", || {
            ts.keep_busy();
            assert_eq!(ts.tabs.len(), 1, "killed mid-line");
            screen(&ts.tabs[0]).contains("finished")
        });
    }

    #[cfg(windows)]
    #[test]
    fn powershell_is_busy_while_a_command_runs() {
        let dir = crate::repo::testutil::scratch("terminal-win");
        let ctx = egui::Context::default();
        let mut ts = Terminals::default();
        ts.toggle(&ctx, &dir);
        assert!(ts.open && ts.tabs.len() == 1, "{:?}", ts.error);
        let s = &ts.tabs[0];
        // The echoed command line shows the pieces; only the output joins them.
        s.write(&b"echo ('re'+'ady')\r"[..]);
        wait("prompt", || screen(s).contains("ready") && !s.busy());
        s.write(&b"ping -n 6 127.0.0.1; echo ('do'+'ne')\r"[..]);
        wait("busy", || s.busy());
        wait("done", || screen(s).contains("done"));
        wait("idle", || !s.busy());
        ts.close(0);
        assert!(!ts.open && ts.tabs.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
