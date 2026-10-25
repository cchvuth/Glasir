# Glasir feature map

Generated from `docs/features.json` by `tools/featuremap/featuremap.py render`; edit the JSON, not this file.
Every non-test function belongs to at least one feature (`featuremap.py check`). `featuremap.py affected HEAD~1..HEAD` lists the features a change touches.

## Contents

- [App shell](#app) (58 features)
- [Committing and staging](#commit) (43 features)
- [Branches, tags and the sidebar](#refs) (60 features)
- [History rewriting and stacks](#rewrite) (21 features)
- [Merging, cherry-picks and conflicts](#merge) (28 features)
- [Remotes, sync and hosting](#remote) (33 features)
- [Graph, inspector and views](#view) (43 features)
- [Stashes, worktrees, undo and tools](#tools) (41 features)
- [Platform](#platform) (12 features)

## Most shared functions

Changing one of these means re-reviewing every feature listed for it in `features.json`.

| Function | Features |
|---|---|
| `ui::inspector::wip::RepoTab::wip_panel` (src/ui/inspector/wip.rs:280) | 31 |
| `ui::palette::items` (src/ui/palette.rs:74) | 25 |
| `ui::graph_view::menus::commit_menu` (src/ui/graph_view/menus.rs:141) | 24 |
| `ui::sidebar::ref_menus::local_menu` (src/ui/sidebar/ref_menus.rs:184) | 22 |
| `repo::RepoTab::apply` (src/repo/mod.rs:653) | 21 |
| `ui::diff_view::show` (src/ui/diff_view.rs:217) | 20 |
| `repo::action::DiffSrc::change` (src/repo/action.rs:34) | 20 |
| `ui::graph_view::show` (src/ui/graph_view/mod.rs:129) | 17 |
| `repo::data::RepoData::split_remote` (src/repo/data.rs:295) | 13 |
| `ui::titlebar::App::tab_bar` (src/ui/titlebar.rs:11) | 12 |
| `repo::rebase::head_state` (src/repo/rebase.rs:584) | 12 |
| `ui::sidebar::ref_menus::tag_menu` (src/ui/sidebar/ref_menus.rs:336) | 11 |
| `ui::diff_view::paint_line` (src/ui/diff_view.rs:819) | 11 |
| `ui::app::App::shortcuts` (src/ui/app.rs:192) | 11 |
| `repo::op::exec_stash` (src/repo/op.rs:836) | 11 |
| `ui::settings::App::settings_dialog` (src/ui/settings.rs:134) | 10 |
| `ui::diff_view::rows` (src/ui/diff_view.rs:29) | 10 |
| `ui::diff_view::row_kind` (src/ui/diff_view.rs:92) | 10 |
| `ui::diff_view::row_changes` (src/ui/diff_view.rs:602) | 10 |
| `ui::diff_view::pick_lines` (src/ui/diff_view.rs:614) | 10 |
| `ui::diff_view::Layout::get` (src/ui/diff_view.rs:75) | 10 |
| `ui::app::App::ui` (src/ui/app.rs:519) | 10 |
| `ui::sidebar::ref_menus::remote_menu` (src/ui/sidebar/ref_menus.rs:307) | 9 |
| `ui::inspector::tree::visible` (src/ui/inspector/tree.rs:106) | 9 |
| `ui::diff_view::apply_op` (src/ui/diff_view.rs:155) | 9 |

<a id="app"></a>
## App shell

### Startup & window

#### Start up and import the login shell environment `app.startup`
- **Entry:** src/main.rs:18 process start (also GLASIR_ASKPASS dispatch to askpass::run, Linux desktop entry: app.linux-install); src/main.rs:44 import_login_env (unix) so GUI launches see PATH, SSH_AUTH_SOCK
- **Platform:** `platform.git-runner`
- **Shared:** `git::env::import_login_env` (3), `git::env::parse_env` (3), `main` (5)

#### Warn when git is missing or too old `app.git-version-check`
- **Entry:** src/ui/app.rs:91 git_check task at startup; src/ui/app.rs:557 dismissible warning banner under the tab bar (x pill)
- **Platform:** `platform.task`
- **Shared:** `git::env::check_git` (3), `git::env::parse_version` (3), `ui::app::App::new` (8), `ui::app::App::ui` (10)

#### Open a repository passed on the command line `app.cli-open`
- **Entry:** src/main.rs:19 first CLI argument; src/ui/app.rs:120 App::new -> open
- **Shared:** `main` (5), `ui::app::App::new` (8), `ui::app::App::open` (5)

#### Native macOS Liquid Glass backdrop / transparent window `app.native-glass`
- **Entry:** src/ui/app.rs:71 App::new (Settings -> Window glass); src/main.rs:55 transparent viewport; startup only
- **Platform:** `platform.design-system`
- **Own code:** `ui::app::App::clear_color`
- **Shared:** `ui::app::App::new` (8), `ui::app::native_glass` (2)

#### Per-frame loop: poll all tabs, apply deferred actions, route dialogs `app.frame-loop`
- **Entry:** every frame: App::logic then App::ui; runs while the window is hidden so ops still finish
- **Platform:** `platform.op-runner`, `platform.repo-load`
- **Shared:** `ui::app::App::logic` (4), `ui::app::App::poll_tabs` (8), `ui::app::App::raw_input_hook` (2), `ui::app::App::ui` (10)

#### Zoom in/out/reset (persisted) `app.zoom`
- **Entry:** ⌘+ / ⌘- / ⌘0 (egui built-in); src/ui/app.rs:515 zoom persisted each frame; restored in App::new
- **Shared:** `ui::app::App::new` (8), `ui::app::App::ui` (10)

#### Reload the repo when the window regains focus `app.focus-reload`
- **Entry:** background: window focus event (when the watcher is not running); src/ui/app.rs:523
- **Platform:** `platform.repo-load`, `platform.watcher`
- **Shared:** `ui::app::App::ui` (10)

#### Drive the periodic background fetch per tab `app.auto-fetch-tick`
- **Entry:** background: every frame, interval from Settings -> Auto-fetch, plus on focus; src/ui/app.rs:531 t.auto_fetch (the fetch itself is repo/fetch.rs)
- **Platform:** `platform.task`
- **Shared:** `ui::app::App::ui` (10)

### Updates and install

#### Check GitHub for a newer Glasir release (startup, window refocus) `app.update-check`
- **Entry:** startup: src/ui/app.rs:122 App::new -> check_updates (src/ui/update.rs:26); window regains focus: src/ui/app.rs:521; at most one check per 10 minutes (GitHub allows 60 unauthenticated requests an hour); Settings → Updates "Check for a new version at startup" (src/ui/settings.rs:199); off = no request at all; background: src/update.rs:50 check asks api.github.com for cchvuth/Glasir's latest release, compares versions numerically (newer) and picks this platform's asset; also clears a finished update's leftovers; offline / rate-limited stays silent; GLASIR_SCREENSHOT_VIEW=update shows the pill (src/ui/screenshot.rs)
- **Platform:** `platform.task`
- **Own code:** `ui::update::App::check_updates`, `update::check`, `update::parse`, `update::newer`, `update::clear_stale`
- **Shared:** `ui::update::App::update_pill` (2), `update::agent` (2), `update::asset_name` (2), `update::target` (3), `update::stage` (2), `update::clear` (2), `ui::app::App::new` (8), `ui::app::App::ui` (10), `ui::settings::App::settings_dialog` (10), `ui::settings::Settings::default` (2)

#### Update Glasir in place from the title bar (download, swap, restart) `app.self-update`
- **Entry:** title bar pill "Update to <version>" (src/ui/titlebar.rs:35 -> src/ui/update.rs:79 update_pill), accent-filled with a pulsing glow; click downloads and installs (src/update.rs:94 install: the release asset for this OS, unpacked with ditto / tar next to the app, swapped in keeping the old copy until the swap worked); right-click the pill: "Release notes" (src/ui/update.rs:91); a copy that can't update itself (cargo target build, the .deb's /usr/bin/glasir, no asset for this platform): the pill opens the release page (src/update.rs:61 target); installed: Glasir closes through the usual quit guards and the new version starts once this process has exited (src/ui/update.rs:37 restart -> src/update.rs:130 relaunch); "Restart to update" pill while a guard holds the quit (src/ui/update.rs:66); failure: toast "Update failed: …" with the release page
- **Platform:** `platform.task`, `platform.design-system`
- **Own code:** `ui::update::App::restart`, `update::install`, `update::install_to`, `update::unpack`, `update::swap`, `update::relaunch`
- **Shared:** `ui::update::App::update_pill` (2), `update::stage` (2), `update::clear` (2), `update::target` (3), `update::asset_name` (2), `update::agent` (2), `ui::titlebar::App::tab_bar` (12)

#### Linux launcher entry and icon (Glasir --install, refreshed on every start) `app.linux-install`
- **Entry:** `Glasir --install` (src/main.rs:37, the README's install command): writes the entry and icon, waits for the file-manager icon, exits without opening a window; every start on Linux (src/main.rs:38 -> :70 desktop_entry): (re)writes ~/.local/share/applications/Glasir.desktop and icons/hicolor/512x512/apps/glasir.png when missing or when the binary moved, so launchers and Wayland docks show the icon; sets the binary's icon in GNOME Files via gio metadata::custom-icon; skipped for the .deb's /usr/bin/glasir (the package ships its own entry), which instead removes a user entry whose binary is gone (src/main.rs exec_target); the Exec path is quoted and escaped per the Desktop Entry spec (src/main.rs exec_value); a relative XDG_DATA_HOME is ignored
- **Own code:** `exec_value`, `exec_target`
- **Shared:** `desktop_entry` (2), `main` (5)

#### App icon: window icon, Windows .exe icon, app id `app.app-icon`
- **Entry:** window / taskbar icon from assets/icon-512.png and app id "Glasir" (src/main.rs:50-51; Wayland matches it to the launcher entry's StartupWMClass); Windows Explorer and pinned taskbar: assets/icon.ico embedded in Glasir.exe at build time (build.rs:7 via winresource; build.rs is outside the function inventory)
- **Shared:** `main` (5)

#### Debian / Ubuntu .deb package `app.deb-package`
- **Entry:** scripts/linux-deb.sh [dist] -> dist/Glasir-linux-amd64.deb: /usr/bin/glasir, /usr/share/applications/Glasir.desktop, the 512 px icon, Depends: git, libc6 (>= 2.35) (shell script, no Rust functions; the Linux release is built on ubuntu-22.04 for that glibc); the installed /usr/bin copy gets no per-user launcher entry, a stale one is removed (src/main.rs:71 desktop_entry) and does not update itself: the update pill opens the release page (src/update.rs:61 target)
- **Shared:** `desktop_entry` (2), `update::target` (3)

### Tabs

#### Open a repository `app.open-repo`
- **Entry:** ⌘O; Title bar + button on the welcome tab (src/ui/titlebar.rs:123 'Open a repository'); Welcome 'Open repository…' (src/ui/welcome.rs:39); ⌘P 'Open repository…' (Cmd::PickOpen); ⌘P recent repository items (Cmd::Open); drag a folder or file onto the window; CLI argument
- **Platform:** `platform.git-runner`, `platform.repo-load`
- **Own code:** `ui::app::App::pick_and_open`
- **Shared:** `ui::app::App::add_tab` (3), `ui::app::App::command_palette` (4), `ui::app::App::new_tab` (3), `ui::app::App::open` (5), `ui::app::App::remember` (3), `ui::app::App::shortcuts` (11), `ui::app::App::ui` (10), `ui::app::drop_hint` (2)

#### New tab (welcome page) `app.new-tab`
- **Entry:** ⌘T; Title bar + button when a repo tab is active (src/ui/titlebar.rs:123 'New tab (⌘T)')
- **Shared:** `ui::app::App::shortcuts` (11), `ui::titlebar::App::tab_bar` (12)

#### Switch tabs `app.switch-tab`
- **Entry:** click a tab (src/ui/titlebar.rs); ⌘1..⌘8 / ⌘9 (last tab); ⌃Tab / ⌃⇧Tab
- **Shared:** `ui::app::App::shortcuts` (11), `ui::titlebar::App::tab_bar` (12), `ui::titlebar::tab_label` (2)

#### Close a tab `app.close-tab`
- **Entry:** ⌘W; x pill on the tab (titlebar.rs:53 'Close tab (⌘W)'); refused toast when unsaved edits / running op / terminal / external tool
- **Platform:** `platform.undo-engine`
- **Shared:** `repo::op::RepoTab::draft_message` (5), `ui::app::App::close_tab` (5), `ui::app::App::shortcuts` (11), `ui::app::draft` (5), `ui::app::drop_data_off_thread` (3), `ui::app::forget_draft` (5), `ui::titlebar::App::tab_bar` (12)

#### Close other tabs `app.close-other-tabs`
- **Entry:** Tab context menu 'Close other tabs' (titlebar.rs:83)
- **Shared:** `ui::app::App::close_tab` (5), `ui::titlebar::App::tab_bar` (12)

#### Reorder tabs by dragging `app.reorder-tabs`
- **Entry:** drag a tab in the title bar (titlebar.rs:75)
- **Own code:** `ui::titlebar::move_target`
- **Shared:** `ui::titlebar::App::tab_bar` (12)

#### Rename a tab (alias) `app.rename-tab`
- **Entry:** Tab context menu 'Rename…' (titlebar.rs:79); Modal 'Rename tab'
- **Own code:** `ui::titlebar::App::rename_dialog`
- **Shared:** `ui::titlebar::App::tab_bar` (12)

#### Reveal the tab's repo in the file manager `app.tab-reveal`
- **Entry:** Tab context menu 'Reveal in file manager' (titlebar.rs:87)
- **Shared:** `ui::titlebar::App::tab_bar` (12)

#### Tab labels with alias, branch and change state `app.tab-label`
- **Entry:** title bar tabs (titlebar.rs:140)
- **Platform:** `platform.format`
- **Shared:** `ui::titlebar::App::tab_bar` (12), `ui::titlebar::tab_label` (2)

#### Drag title bar to move window, double-click to zoom `app.titlebar-window`
- **Entry:** empty title bar space (titlebar.rs:12)
- **Shared:** `ui::titlebar::App::tab_bar` (12)

#### Global title bar buttons (settings, command log, palette, light/dark) `app.titlebar-buttons`
- **Entry:** titlebar.rs:22 Settings (⌘,); titlebar.rs:25 Command log (⇧⌘L); titlebar.rs:28 Search commands ⌘P; titlebar.rs:32 Switch light / dark
- **Platform:** `platform.design-system`
- **Shared:** `ui::settings::App::open_settings` (4), `ui::settings::toggle_flavor` (2), `ui::titlebar::App::tab_bar` (12)

#### Restore open tabs and the active tab on launch `app.restore-session`
- **Entry:** startup: Settings.open / active (App::new); saved on quit/periodically by App::save
- **Platform:** `platform.git-runner`, `platform.repo-load`
- **Shared:** `ui::app::App::add_tab` (3), `ui::app::App::new` (8), `ui::app::App::new_tab` (3), `ui::app::App::save` (3)

#### Open a worktree in the current tab (or a new one) `app.switch-worktree-in-tab`
- **Entry:** Action::Switch / Action::OpenTab from sidebar/worktree menus and branch checkout (handled by App::poll_tabs)
- **Platform:** `platform.undo-engine`
- **Shared:** `repo::op::RepoTab::draft_message` (5), `ui::app::App::add_tab` (3), `ui::app::App::poll_tabs` (8), `ui::app::App::remember` (3), `ui::app::App::switch` (4), `ui::app::draft` (5), `ui::app::drop_data_off_thread` (3), `ui::app::forget_draft` (5), `ui::app::replaceable` (2)

#### Keep the typed commit message per repo (survives tab close and restart) `app.commit-draft`
- **Entry:** automatic: closing a tab, switching worktree, quitting (.git/GLASIR_DRAFT)
- **Own code:** `ui::app::saved_text`
- **Shared:** `repo::op::RepoTab::draft_message` (5), `ui::app::App::new_tab` (3), `ui::app::App::persist_drafts` (2), `ui::app::App::save` (3), `ui::app::draft` (5), `ui::app::draft_file` (3), `ui::app::forget_draft` (5)

### Welcome & recent repositories

#### Welcome page `app.welcome`
- **Entry:** shown when no tab is open (src/ui/welcome.rs:29)
- **Shared:** `ui::welcome::App::welcome` (3)

#### Initialize a repository `app.init-repo`
- **Entry:** Welcome 'Init…' button (welcome.rs:46)
- **Platform:** `platform.git-runner`
- **Shared:** `ui::app::App::open` (5), `ui::welcome::App::welcome` (3)

#### Recent repositories list `app.recent-repos`
- **Entry:** Welcome page RECENT list (click to open); right-click 'Remove from recent'; ⌘P recent repo items
- **Shared:** `ui::app::App::open` (5), `ui::app::App::remember` (3), `ui::welcome::App::welcome` (3)

#### First-run tips strip `app.first-run-tips`
- **Entry:** Welcome page tips strip, x 'Hide these tips' (welcome.rs:189)
- **Platform:** `platform.format`
- **Own code:** `ui::welcome::tips`

### Settings

#### Settings dialog `app.settings-dialog`
- **Entry:** ⌘,; Title bar gear (titlebar.rs:22); ⌘P 'Settings'; Settings struct persisted with the app state
- **Platform:** `platform.design-system`
- **Shared:** `ui::app::App::new` (8), `ui::app::App::save` (3), `ui::settings::App::open_settings` (4), `ui::settings::App::settings_dialog` (10), `ui::settings::Settings::default` (2)

#### Theme flavours and light/dark toggle `app.theme-flavour`
- **Entry:** Settings -> Theme swatches; Title bar light/dark pill; ⌘P "Toggle light / dark theme" (Cmd::Theme)
- **Platform:** `platform.design-system`
- **Own code:** `ui::settings::flavor_swatch`, `ui::settings::set_flavor`
- **Shared:** `ui::app::App::command_palette` (4), `ui::settings::App::settings_dialog` (10), `ui::settings::toggle_flavor` (2)

#### Glass transparency and native window glass `app.glass-transparency`
- **Entry:** Settings -> Glass transparency slider and 'Window glass' checkbox
- **Platform:** `platform.design-system`
- **Shared:** `ui::app::native_glass` (2), `ui::settings::App::settings_dialog` (10)

#### General settings (commits to load, default pull, auto-fetch, avatars, update check, stacks, email rules, protected branches) `app.settings-general`
- **Entry:** Settings dialog rows
- **Platform:** `platform.repo-load`
- **Shared:** `ui::app::App::ui` (10), `ui::settings::App::settings_dialog` (10)

#### Editor, terminal and custom commands settings `app.settings-tools`
- **Entry:** Settings -> Tools section
- **Shared:** `ui::settings::App::open_settings` (4), `ui::settings::App::settings_dialog` (10), `ui::settings::tools_section` (2)

#### AI settings: provider, token or command, model, custom instructions, test connection `app.settings-ai`
- **Entry:** Settings -> AI section: Enable AI features, Provider combo box (Claude Code CLI / Codex CLI / Claude API / OpenAI API), token Save / Remove (API providers only), Model, API base URL (or Command for a CLI), Custom instructions presets, Test connection
- **Platform:** `platform.ai-client`, `platform.task`
- **Shared:** `ai::tasks::test` (2), `ui::settings::App::open_settings` (4), `ui::settings::App::settings_dialog` (10), `ui::settings::token_source` (2)

#### AI through the signed-in Claude Code or Codex CLI (default provider, no token) `app.ai-cli-provider`
- **Entry:** Settings → AI → Provider "Claude Code CLI" (the default) or "Codex CLI" (src/ui/settings.rs:263); the token rows are hidden and "API base URL" becomes "Command" (name or full path of the CLI); Model: "default" leaves it to the CLI; Claude Code also offers opus / sonnet / haiku (src/ai/cli.rs:12); every AI feature: ai::client::complete -> src/ai/cli.rs:19 complete runs `claude -p --output-format stream-json` with no tools, no session, no MCP and user settings only (system prompt via --system-prompt-file), or `codex exec --sandbox read-only -c mcp_servers={} --json`, in a fresh private (0700) temp folder removed afterwards, in its own process group (cancel and a 180 s silence timeout kill the whole tree); streamed text (claude) or the last message file (codex); JSON-schema replies via --json-schema / --output-schema; errors: a missing CLI says how to install it or set its path (src/ai/cli.rs:30 spawn; .cmd fallback on Windows); "not logged in" and other CLI errors show as the AI error; Settings → AI "Test connection" reports "<model> replied: …" (ai::tasks::test)
- **Platform:** `platform.ai-client`
- **Shared:** `ai::cli::endpoint` (2), `ai::cli::complete` (2), `ai::cli::spawn` (2), `ai::cli::feed` (2), `ai::cli::stderr_of` (2), `ai::cli::private_dir` (2), `ai::cli::read` (2), `ai::cli::stop` (2), `ai::cli::failed` (2), `ai::cli::claude` (2), `ai::cli::codex` (2), `ai::client::Provider::is_cli` (2), `ai::client::Provider::label` (2), `ai::client::Provider::models` (2), `ai::client::Provider::name` (2), `ai::client::Config::default` (2), `ai::client::Config::current` (2), `ai::client::Config::model` (2), `ai::client::complete` (2), `ai::client::need_token` (2), `ai::client::env_token` (2), `ai::name` (2), `ai::sync` (2), `ai::tasks::test` (2), `ui::settings::App::settings_dialog` (10), `ui::settings::token_source` (2)

### Command palette & shortcuts

#### Command palette (fuzzy search of commands, branches, repos) `app.palette`
- **Entry:** ⌘P / ⌘K; Title bar 'Search commands' pill; items belong to their features' entry lists
- **Platform:** `platform.design-system`
- **Own code:** `ui::palette::elide`, `ui::palette::git`, `ui::palette::rank`, `ui::palette::score`, `ui::palette::show`
- **Shared:** `ui::app::App::command_palette` (4), `ui::app::App::shortcuts` (11), `ui::palette::item` (2), `ui::palette::items` (25)

#### Keyboard shortcut cheat sheet `app.shortcuts-help`
- **Entry:** ? or ⌘/; ⌘P 'Keyboard shortcuts' (Cmd::Shortcuts)
- **Platform:** `platform.format`
- **Own code:** `ui::shortcuts::show`
- **Shared:** `ui::app::App::command_palette` (4), `ui::app::App::shortcuts` (11)

#### Global keyboard shortcuts dispatch `app.global-shortcuts`
- **Entry:** src/ui/app.rs:192 App::shortcuts: ⌘O ⌘, ⌘/ ⇧⌘L ⌘P/K ⌘T ⌘W ⌘1-9 ⌃Tab F5/⌘R ⌘L ⌘Z/⇧⌘Z/⌘Y ⌘F ⇧⌘S ⇧⌘M ⇧⌘U S/U ⌃`
- **Platform:** `platform.op-runner`
- **Shared:** `ui::app::App::shortcuts` (11)

### Notifications, toolbar and layout

#### Toast notifications (with undo button) `app.toasts`
- **Entry:** bottom-right toasts after ops and errors (toasts.rs); click undo arrow in a toast
- **Platform:** `platform.undo-engine`
- **Shared:** `ui::app::App::poll_tabs` (8), `ui::toasts::App::show_toasts` (2), `ui::toasts::App::toast` (2), `ui::toasts::App::toast_undo` (2)

#### Per-repo toolbar layout and buttons `app.toolbar`
- **Entry:** top panel of a repo tab (toolbar.rs): Refresh F5, terminal toggle, open-in menu, web globe, overflow menu, branch/ahead-behind label, Undo/Redo, Fetch/Pull/Push menus, Branch/Stash/Pop, conflict chip, fetch-failed chip, progress/cancel; each button's action is owned by its feature
- **Platform:** `platform.design-system`, `platform.format`
- **Shared:** `ui::toolbar::history_tools` (7), `ui::toolbar::label` (3), `ui::toolbar::open_items` (4), `ui::toolbar::pull_hint` (2), `ui::toolbar::toolbar` (8), `ui::toolbar::toolbar_local` (5), `ui::toolbar::toolbar_remote` (5)

#### Toolbar title: repository · worktree · branch, longest part cut first `app.toolbar-title`
- **Entry:** left card of the repo toolbar (src/ui/toolbar.rs:47-82): the repository name (its primary worktree's folder), the worktree when the repo has several (house = primary, tree = linked, "primary" or the folder name; hover shows its path), then the branch with ↑ / ↓ counts; narrow window: src/ui/toolbar.rs:144 fit cuts the longest part first, down to one common width, each ending in …; short parts stay whole
- **Platform:** `platform.design-system`
- **Own code:** `ui::toolbar::fit`
- **Shared:** `ui::toolbar::toolbar` (8), `ui::sidebar::worktrees::worktree_name` (2)

#### Toolbar Undo / Redo buttons `app.toolbar-undo-redo`
- **Entry:** toolbar.rs Undo / Redo (⌘Z / ⇧⌘Z)
- **Platform:** `platform.undo-engine`
- **Shared:** `ui::toolbar::toolbar` (8)

#### Repo view layout (toolbar, sidebar, graph, details, center-pane switch, terminal panel) `app.repo-layout`
- **Entry:** every repo tab (repo_view.rs:12)
- **Platform:** `platform.design-system`, `platform.op-runner`
- **Shared:** `ui::repo_view::show` (6)

#### 'Optimize this repository' hint banner `app.optimize-hint`
- **Entry:** banner above the graph when repo health is poor (repo_view.rs:149)
- **Shared:** `ui::repo_view::optimize_hint` (3)

#### Drop-a-folder overlay `app.drop-hint`
- **Entry:** hover a file/folder over the window (app.rs drop_hint)
- **Shared:** `ui::app::drop_hint` (2)

### Quit & close guards

#### Hold window close until the running op finishes `app.close-while-busy`
- **Entry:** close window / ⌘Q while an op runs (toast 'Quitting once … finishes')
- **Platform:** `platform.op-runner`
- **Own code:** `ui::app::hold_close`
- **Shared:** `repo::RepoTab::busy` (2), `ui::app::App::logic` (4), `ui::app::refuse_close` (3)

#### Refuse close/quit with unsaved work `app.unsaved-guard`
- **Entry:** close window, ⌘W, ⌘Q with unsaved file edits, merge resolution, rebase plan, composer plan, split in progress
- **Own code:** `repo::RepoTab::unsaved`
- **Shared:** `ui::app::App::close_tab` (5), `ui::app::App::guard_close` (4), `ui::app::App::logic` (4), `ui::app::refuse_close` (3)

#### Confirm quitting with running terminal commands / external tools `app.quit-terminal-confirm`
- **Entry:** modal 'Quit Glasir?' (app.rs:385)
- **Shared:** `ui::app::App::guard_close` (4), `ui::app::App::quit_dialog` (3), `ui::app::refuse_close` (3)

#### macOS ⌘Q / Dock Quit routed through the close guards `app.mac-quit`
- **Entry:** macOS applicationShouldTerminate: hook (mac_quit)
- **Own code:** `ui::app::mac_quit::install`, `ui::app::mac_quit::should_terminate`
- **Shared:** `ui::app::App::logic` (4), `ui::app::App::new` (8)

### Dialog framework

#### Prompt dialog (asks for a value, substitutes {} in the op) `app.dialog-prompt`
- **Entry:** Action::Prompt / repo::prompt used by many features (new branch, tag, rename…)
- **Platform:** `platform.op-runner`
- **Shared:** `repo::action::prompt` (7), `ui::dialogs::fill` (8), `ui::dialogs::fill_arg` (7), `ui::dialogs::prompt_dialog` (8)

#### Confirm dialog for destructive ops `app.dialog-confirm`
- **Entry:** Op::confirm(text) shows this before running
- **Platform:** `platform.op-runner`
- **Own code:** `ui::dialogs::confirm_dialog`
- **Shared:** `ui::dialogs::fill` (8)

#### Failed-op choice dialog `app.dialog-choice`
- **Entry:** shown when platform.offer maps a failure to choices (e.g. autostash, force push)
- **Platform:** `platform.offer`
- **Own code:** `ui::dialogs::choice_dialog`

#### AI result dialog (stream, copy, regenerate, apply) `app.ai-result-dialog`
- **Entry:** any AI action: explain, PR description, resolve conflict, WIP explain; buttons Open pull request page / Copy / Apply & mark resolved / Regenerate / Close
- **Platform:** `platform.ai-client`, `platform.op-runner`
- **Shared:** `ui::dialogs::ai_dialog` (4)

### Developer harness

#### Screenshot harness `app.screenshot-harness`
- **Entry:** GLASIR_SCREENSHOT=<bmp> GLASIR_SCREENSHOT_VIEW=… (settings, log, update = the update pill, terminal-off, diff-staged:<path>, scroll-bench, …) GLASIR_SCREENSHOT_WIDTH GLASIR_KEYS
- **Own code:** `ui::screenshot::App::screenshot`, `ui::screenshot::shot_input`
- **Shared:** `ui::app::App::raw_input_hook` (2)

#### Frame-time / slow git command reporting `app.perf-harness`
- **Entry:** GLASIR_PERF=1 (report_perf every 2 s; git::log::record prints >30 ms commands); GLASIR_SCREENSHOT_VIEW=scroll-bench
- **Own code:** `ui::app::App::report_perf`

<a id="commit"></a>
## Committing and staging

### Staging and unstaging files

#### Stage a file `commit.stage-file`
- **Entry:** src/ui/inspector/wip.rs:613 hover "+" button on an unstaged row; double-click an unstaged row; row context menu "Stage" (wip.rs:640); src/ui/diff_view.rs:308 "Stage file" button in an open unstaged diff; S key with files selected (app.rs:281 -> RepoTab::file_key)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::action::DiffSrc::change` (20), `repo::op::stage_files_op` (5), `repo::op::without_vanished` (6), `ui::diff_view::show` (20), `ui::inspector::file_row` (6), `ui::inspector::hovering` (5), `ui::inspector::row_button` (5), `ui::inspector::tree::Order::place` (7), `ui::inspector::tree::dir_row` (8), `ui::inspector::tree::indent` (7), `ui::inspector::tree::list` (7), `ui::inspector::tree::show` (7), `ui::inspector::tree::toggle` (7), `ui::inspector::tree::visible` (9), `ui::inspector::wip::RepoTab::file_key` (3), `ui::inspector::wip::RepoTab::wip_panel` (31), `ui::inspector::wip::SelCache::get` (4), `ui::inspector::wip::SelCache::has` (4), `ui::inspector::wip::pick_file` (4), `ui::inspector::wip::selected` (4)

#### Unstage a file `commit.unstage-file`
- **Entry:** wip.rs:710 hover "-" button on a staged row; double-click a staged row; row menu "Unstage" (wip.rs:731); diff_view.rs:305 "Unstage file" in a staged diff; U key with staged files selected
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::op::unstage_op`
- **Shared:** `repo::action::DiffSrc::change` (20), `repo::op::unstage_files_op` (3), `ui::diff_view::show` (20), `ui::inspector::file_row` (6), `ui::inspector::hovering` (5), `ui::inspector::row_button` (5), `ui::inspector::tree::Order::place` (7), `ui::inspector::tree::dir_row` (8), `ui::inspector::tree::indent` (7), `ui::inspector::tree::list` (7), `ui::inspector::tree::show` (7), `ui::inspector::tree::toggle` (7), `ui::inspector::tree::visible` (9), `ui::inspector::wip::RepoTab::file_key` (3), `ui::inspector::wip::RepoTab::wip_panel` (31), `ui::inspector::wip::SelCache::get` (4), `ui::inspector::wip::SelCache::has` (4), `ui::inspector::wip::pick_file` (4), `ui::inspector::wip::selected` (4)

#### Stage all changes `commit.stage-all`
- **Entry:** wip.rs:576 "Stage all" button (Unstaged files header); ⇧⌘S shortcut (app.rs:269); ⌘P "Stage all changes" (palette.rs:108)
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::op::stage_all_op` (2), `repo::op::stage_files_op` (5), `repo::op::without_vanished` (6), `ui::inspector::wip::RepoTab::wip_panel` (31)

#### Unstage all changes `commit.unstage-all`
- **Entry:** wip.rs:680 "Unstage all" button (Staged files header); ⇧⌘U shortcut (app.rs:276); ⌘P "Unstage all changes" (palette.rs:111)
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::op::unstage_all_op`
- **Shared:** `ui::inspector::wip::RepoTab::wip_panel` (31)

#### Select several WIP files and act on them `commit.multi-select`
- **Entry:** click / ⌘-click / ⇧-click on rows in the Unstaged and Staged lists (wip.rs pick_file); "Stage N" / "Discard N" / "Unstage N" buttons in the list headers; row context menu "Stage N files" / "Discard N files" / "Unstage N files"; S / U keys (app.rs:281)
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::op::discard_files_op` (4), `repo::op::discard_many` (3), `repo::op::stage_files_op` (5), `repo::op::unstage_files_op` (3), `repo::op::without_vanished` (6), `ui::inspector::file_row` (6), `ui::inspector::hovering` (5), `ui::inspector::row_button` (5), `ui::inspector::tree::Order::place` (7), `ui::inspector::tree::dir_row` (8), `ui::inspector::tree::indent` (7), `ui::inspector::tree::list` (7), `ui::inspector::tree::show` (7), `ui::inspector::tree::toggle` (7), `ui::inspector::tree::visible` (9), `ui::inspector::wip::RepoTab::file_key` (3), `ui::inspector::wip::RepoTab::wip_panel` (31), `ui::inspector::wip::SelCache::get` (4), `ui::inspector::wip::SelCache::has` (4), `ui::inspector::wip::pick_file` (4), `ui::inspector::wip::selected` (4), `ui::inspector::wip::stash_files_item` (3)

#### Stage / unstage a folder in tree view `commit.folder-ops`
- **Entry:** hover "+"/"-" on a folder row in the WIP tree (wip.rs:589, 692); folder row context menu "Stage folder" / "Unstage folder" / "Discard changes in folder"
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::op::discard_checkout` (3), `repo::op::discard_files_op` (4), `repo::op::discard_many` (3), `repo::op::discard_op` (5), `repo::op::intent` (4), `repo::op::stage_files_op` (5), `repo::op::unstage_files_op` (3), `repo::op::without_vanished` (6), `ui::inspector::tree::Order::place` (7), `ui::inspector::tree::dir_row` (8), `ui::inspector::tree::indent` (7), `ui::inspector::tree::list` (7), `ui::inspector::tree::show` (7), `ui::inspector::tree::toggle` (7), `ui::inspector::tree::visible` (9), `ui::inspector::wip::RepoTab::wip_panel` (31), `ui::inspector::wip::dir_items` (3)

#### Switch the WIP file lists between flat list and folder tree `commit.tree-toggle`
- **Entry:** list/tree toggle icon in the Unstaged / Staged / Conflicts headers (tree::toggle); folder row click to collapse/expand
- **Platform:** `platform.design-system`
- **Shared:** `ui::inspector::file_row` (6), `ui::inspector::tree::Order::place` (7), `ui::inspector::tree::dir_row` (8), `ui::inspector::tree::indent` (7), `ui::inspector::tree::list` (7), `ui::inspector::tree::show` (7), `ui::inspector::tree::toggle` (7), `ui::inspector::tree::visible` (9), `ui::inspector::wip::RepoTab::wip_panel` (31)

### Discarding changes

#### Discard changes to a file `commit.discard-file`
- **Entry:** unstaged row context menu "Discard changes" (wip.rs:663)
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::op::discard_checkout` (3), `repo::op::discard_files_op` (4), `repo::op::discard_op` (5), `repo::op::intent` (4), `ui::inspector::wip::RepoTab::wip_panel` (31)

#### Discard several files at once `commit.discard-selected`
- **Entry:** "Discard N" in Unstaged header (wip.rs:572); context menu "Discard N files" (wip.rs:634); folder menu "Discard changes in folder"
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::op::discard_checkout` (3), `repo::op::discard_files_op` (4), `repo::op::discard_many` (3), `repo::op::intent` (4), `ui::inspector::wip::RepoTab::wip_panel` (31)

#### Discard all changes `commit.discard-all`
- **Entry:** graph WIP row context menu "Discard all changes" (graph_view/menus.rs:338); ⌘P / undo-tested Op DISCARD_ALL
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::op::discard_all`
- **Shared:** `ui::graph_view::menus::discard_all_op` (2)

### Hunks and lines

#### Open a WIP file diff (unstaged / staged) `commit.view-worktree-diff`
- **Entry:** click a WIP row (Action::Diff); diff pane in repo_view.rs:76/100; diff options toolbar: split / wrap / ignore whitespace / context (view_controls)
- **Platform:** `platform.design-system`, `platform.git-runner`, `platform.highlight`, `platform.task`
- **Own code:** `git::diff::DiffOpts::args`, `git::diff::DiffOpts::default`, `git::diff::b_path`, `git::diff::blob_size`, `git::diff::file_paths`, `git::diff::file_size`, `git::diff::large`, `git::diff::parse_diff`, `git::diff::parse_one`, `git::diff::untracked_diff`, `git::diff::worktree_diff`, `repo::diff::DiffState::loading`, `repo::diff::RepoTab::open_diff`, `repo::diff::RepoTab::poll_diff`, `repo::diff::RepoTab::refresh_diff_theme`, `repo::diff::stamp`
- **Shared:** `git::diff::FileDiff::mode_change` (2), `repo::action::DiffSrc::change` (20), `repo::diff::RepoTab::refresh_diff` (2), `ui::diff_view::Layout::get` (10), `ui::diff_view::change_starts` (2), `ui::diff_view::changed_span` (3), `ui::diff_view::large_note` (2), `ui::diff_view::lfs_note` (3), `ui::diff_view::minimap` (2), `ui::diff_view::note_frame` (2), `ui::diff_view::paint_line` (11), `ui::diff_view::partners` (3), `ui::diff_view::row_kind` (10), `ui::diff_view::rows` (10), `ui::diff_view::show` (20), `ui::diff_view::step` (2), `ui::diff_view::submodule_note` (3), `ui::diff_view::view_controls` (3), `ui::diff_view::wrap_before` (3), `ui::diff_view::wrap_group` (3)

#### Stage a hunk `commit.stage-hunk`
- **Entry:** "Stage hunk" button on a hunk header in an unstaged diff (diff_view.rs:547)
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `git::diff::partial_patch` (8), `git::diff::plain_header` (8), `git::diff::selects_all` (9), `repo::action::DiffSrc::change` (20), `repo::diff::diff_is` (9), `repo::diff::patch_op` (8), `repo::diff::shape` (9), `ui::diff_view::Layout::get` (10), `ui::diff_view::apply_op` (9), `ui::diff_view::paint_line` (11), `ui::diff_view::pick_lines` (10), `ui::diff_view::row_changes` (10), `ui::diff_view::row_kind` (10), `ui::diff_view::rows` (10), `ui::diff_view::show` (20), `ui::diff_view::stage` (4)

#### Unstage a hunk `commit.unstage-hunk`
- **Entry:** "Unstage hunk" button in a staged diff (diff_view.rs:544)
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `git::diff::partial_patch` (8), `git::diff::plain_header` (8), `git::diff::selects_all` (9), `repo::action::DiffSrc::change` (20), `repo::diff::diff_is` (9), `repo::diff::patch_op` (8), `repo::diff::shape` (9), `ui::diff_view::Layout::get` (10), `ui::diff_view::apply_op` (9), `ui::diff_view::paint_line` (11), `ui::diff_view::pick_lines` (10), `ui::diff_view::row_changes` (10), `ui::diff_view::row_kind` (10), `ui::diff_view::rows` (10), `ui::diff_view::show` (20)

#### Discard a hunk `commit.discard-hunk`
- **Entry:** "Discard hunk" button in an unstaged diff (diff_view.rs:550)
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `git::diff::partial_patch` (8), `git::diff::plain_header` (8), `git::diff::selects_all` (9), `repo::action::DiffSrc::change` (20), `repo::diff::diff_is` (9), `repo::diff::patch_op` (8), `repo::diff::shape` (9), `repo::op::discard_op` (5), `ui::diff_view::Layout::get` (10), `ui::diff_view::apply_op` (9), `ui::diff_view::discard` (4), `ui::diff_view::paint_line` (11), `ui::diff_view::pick_lines` (10), `ui::diff_view::row_changes` (10), `ui::diff_view::row_kind` (10), `ui::diff_view::rows` (10), `ui::diff_view::show` (20)

#### Stage selected lines `commit.stage-lines`
- **Entry:** click / drag line numbers or lines in an unstaged diff, then "Stage N line(s)" in the diff header
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `git::diff::partial_patch` (8), `git::diff::plain_header` (8), `git::diff::selects_all` (9), `repo::action::DiffSrc::change` (20), `repo::diff::diff_is` (9), `repo::diff::patch_op` (8), `repo::diff::shape` (9), `ui::diff_view::Layout::get` (10), `ui::diff_view::apply_op` (9), `ui::diff_view::paint_line` (11), `ui::diff_view::pick_lines` (10), `ui::diff_view::row_changes` (10), `ui::diff_view::row_kind` (10), `ui::diff_view::rows` (10), `ui::diff_view::show` (20), `ui::diff_view::stage` (4)

#### Unstage selected lines `commit.unstage-lines`
- **Entry:** select lines in a staged diff, "Unstage N line(s)"
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `git::diff::partial_patch` (8), `git::diff::plain_header` (8), `git::diff::selects_all` (9), `repo::action::DiffSrc::change` (20), `repo::diff::diff_is` (9), `repo::diff::patch_op` (8), `repo::diff::shape` (9), `ui::diff_view::Layout::get` (10), `ui::diff_view::apply_op` (9), `ui::diff_view::paint_line` (11), `ui::diff_view::pick_lines` (10), `ui::diff_view::row_changes` (10), `ui::diff_view::row_kind` (10), `ui::diff_view::rows` (10), `ui::diff_view::show` (20)

#### Discard selected lines `commit.discard-lines`
- **Entry:** select lines in an unstaged diff, "Discard N line(s)"
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `git::diff::partial_patch` (8), `git::diff::plain_header` (8), `git::diff::selects_all` (9), `repo::action::DiffSrc::change` (20), `repo::diff::diff_is` (9), `repo::diff::patch_op` (8), `repo::diff::shape` (9), `repo::op::discard_op` (5), `ui::diff_view::Layout::get` (10), `ui::diff_view::apply_op` (9), `ui::diff_view::discard` (4), `ui::diff_view::paint_line` (11), `ui::diff_view::pick_lines` (10), `ui::diff_view::row_changes` (10), `ui::diff_view::row_kind` (10), `ui::diff_view::rows` (10), `ui::diff_view::show` (20)

#### Stage part of an untracked file (intent-to-add) `commit.partial-untracked`
- **Entry:** stage hunk/lines on a new untracked file: add -N then apply (repo/diff.rs patch_op); discard of an intent-to-add file removes it (op.rs discard_op / intent)
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `git::diff::partial_patch` (8), `git::diff::plain_header` (8), `git::diff::selects_all` (9), `repo::action::DiffSrc::change` (20), `repo::diff::diff_is` (9), `repo::diff::patch_op` (8), `repo::diff::shape` (9), `repo::op::discard_op` (5), `repo::op::intent` (4), `ui::diff_view::Layout::get` (10), `ui::diff_view::apply_op` (9), `ui::diff_view::discard` (4), `ui::diff_view::paint_line` (11), `ui::diff_view::pick_lines` (10), `ui::diff_view::row_changes` (10), `ui::diff_view::row_kind` (10), `ui::diff_view::rows` (10), `ui::diff_view::show` (20), `ui::diff_view::stage` (4)

#### Line-staging disabled notes and stale-diff guard `commit.line-staging-guards`
- **Entry:** "Line staging off" labels in the diff header (ignore-whitespace, CRLF/LFS filter, non-UTF-8, typechange); DIFF_IS guard refuses a patch whose diff went stale (diff.rs diff_is)
- **Platform:** `platform.git-runner`
- **Shared:** `git::diff::FileDiff::mode_change` (2), `git::diff::selects_all` (9), `repo::action::DiffSrc::change` (20), `repo::diff::RepoTab::refresh_diff` (2), `repo::diff::diff_is` (9), `repo::diff::shape` (9), `ui::diff_view::show` (20)

#### Revert / cherry-pick a hunk or lines of a commit diff into the working tree `commit.commit-hunk-apply`
- **Entry:** "Revert hunk" / "Cherry-pick hunk" on a commit diff hunk header; "Revert N lines" / "Cherry-pick N lines" in the diff header
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Own code:** `git::diff::commit_diff`, `repo::diff::apply_hunk`, `repo::diff::hunk_op`
- **Shared:** `git::diff::partial_patch` (8), `git::diff::plain_header` (8), `git::diff::selects_all` (9), `repo::action::DiffSrc::change` (20), `repo::diff::diff_is` (9), `repo::diff::patch_op` (8), `repo::diff::shape` (9), `ui::diff_view::Layout::get` (10), `ui::diff_view::apply_op` (9), `ui::diff_view::paint_line` (11), `ui::diff_view::pick_lines` (10), `ui::diff_view::row_changes` (10), `ui::diff_view::row_kind` (10), `ui::diff_view::rows` (10), `ui::diff_view::show` (20)

### Committing

#### Commit staged changes `commit.commit`
- **Entry:** "Commit N changes to M files" primary button (wip.rs:415); ⌘↩ in the commit box; summary field Enter moves to description
- **Platform:** `platform.offer`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::data::RepoData::replaying`
- **Shared:** `repo::message::add_coauthors` (3), `repo::message::body` (2), `repo::message::is_trailer` (2), `repo::message::strip_comments` (4), `repo::op::RepoTab::commit_op` (6), `repo::op::RepoTab::prefill_template` (3), `repo::op::RepoTab::reset_message` (6), `theme::glass::description` (2), `theme::glass::primary` (2), `theme::glass::small` (2), `ui::inspector::wip::RepoTab::wip_panel` (31)

#### Stage everything and commit `commit.stage-and-commit`
- **Entry:** ⇧⌘↩ in the commit box (wip.rs:435; Stage all then commit_next)
- **Platform:** `platform.op-runner`
- **Shared:** `repo::op::RepoTab::commit_op` (6), `repo::op::RepoTab::reset_message` (6), `repo::op::stage_all_op` (2), `repo::op::stage_files_op` (5), `repo::op::without_vanished` (6), `ui::inspector::wip::RepoTab::wip_panel` (31)

#### Amend previous commit `commit.amend`
- **Entry:** "Amend previous commit" checkbox (wip.rs:386) -> Action::AmendToggled; confirmation when the amended commit is already pushed
- **Platform:** `platform.op-runner`, `platform.task`, `platform.undo-engine`
- **Own code:** `repo::op::RepoTab::fill_amend`, `repo::op::RepoTab::poll_amend`, `repo::op::RepoTab::unfill_amend`, `repo::op::amend_rewrites_pushed`
- **Shared:** `repo::message::add_coauthors` (3), `repo::message::body` (2), `repo::message::split` (2), `repo::message::strip_comments` (4), `repo::op::RepoTab::commit_op` (6), `repo::op::RepoTab::reset_message` (6), `ui::inspector::wip::RepoTab::wip_panel` (31)

#### Commit options: sign, skip hooks `commit.commit-options`
- **Entry:** "Sign commit" checkbox (wip.rs:390); "Skip hooks" checkbox (wip.rs:402) -> git commit --no-verify
- **Platform:** `platform.op-runner`
- **Shared:** `repo::op::RepoTab::commit_op` (6), `repo::op::RepoTab::reset_message` (6), `ui::inspector::wip::RepoTab::wip_panel` (31)

#### Push after commit `commit.push-after-commit`
- **Entry:** "Push after commit" checkbox (wip.rs:404); push runs as a separate op after the commit
- **Platform:** `platform.offer`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::op::RepoTab::commit_op` (6), `repo::op::RepoTab::reset_message` (6), `ui::inspector::wip::RepoTab::wip_panel` (31)

#### Commit message template (commit.template) `commit.template`
- **Entry:** prefilled into the empty commit box (on load / after each commit); untouched template counts as empty
- **Platform:** `platform.repo-load`
- **Own code:** `repo::message::template`
- **Shared:** `repo::message::strip_comments` (4), `repo::op::RepoTab::prefill_template` (3), `ui::inspector::wip::RepoTab::wip_panel` (31)

#### Co-authors (Co-authored-by trailers) `commit.coauthors`
- **Entry:** "Co-authors" menu in the commit box (wip.rs:406) with people from the loaded log or typed Name <email>
- **Platform:** `platform.design-system`, `platform.repo-load`
- **Own code:** `repo::message::parse_person`, `ui::inspector::wip::coauthors_menu`
- **Shared:** `repo::message::add_coauthors` (3), `repo::message::is_trailer` (2), `repo::op::RepoTab::commit_op` (6), `repo::op::RepoTab::reset_message` (6)

#### Commit identity row and email-domain guard `commit.identity-guard`
- **Entry:** identity row under the commit box (wip.rs:367): click to set email, right-click to set name; warning + "Set for this repo…" when a Settings email rule is broken
- **Platform:** `platform.git-model`, `platform.op-runner`
- **Own code:** `repo::identity::mismatch`, `repo::identity::rules`, `repo::identity::suggest`, `repo::identity::under`, `ui::inspector::wip::identity_row`

#### Detached HEAD warning with "Create branch here…" `commit.detached-head-warning`
- **Entry:** warning above the commit box when HEAD is detached (wip.rs:359)
- **Platform:** `platform.op-runner`
- **Shared:** `ui::inspector::wip::RepoTab::wip_panel` (31)

#### Commit message drafts (kept per repo across tabs and restarts) `commit.drafts`
- **Entry:** automatic: when switching worktree in a tab or closing a tab (src/ui/app.rs:174, :306) and on save/exit (persist_drafts) to .git/GLASIR_DRAFT; ⇧⌘M focuses the summary and selects WIP (app.rs:272)
- **Platform:** `platform.repo-load`
- **Shared:** `repo::message::strip_comments` (4), `repo::op::RepoTab::draft_message` (5), `repo::op::RepoTab::prefill_template` (3), `ui::app::App::persist_drafts` (2), `ui::app::draft` (5), `ui::app::draft_file` (3), `ui::app::forget_draft` (5)

#### Write the commit message with AI `commit.ai-commit-message`
- **Entry:** "Generate" (sparkle) button in the commit box (wip.rs:345), Cancel while writing; ⌘P "Write commit message with AI" (palette.rs:119)
- **Platform:** `platform.ai-client`, `platform.task`
- **Shared:** `ai::tasks::Job::poll` (3), `ai::tasks::Job::running` (3), `ai::tasks::Purpose::title` (3), `ai::tasks::build` (5), `ai::tasks::diff_with_stat` (2), `ai::tasks::parent_or_empty` (2), `ai::tasks::recent_subjects` (2), `ai::tasks::repo_instructions` (2), `ai::tasks::secret` (2), `ai::tasks::start` (5), `ai::tasks::system_with` (2), `ai::tasks::too_large` (2), `repo::ai_job::RepoTab::ai_busy` (3), `repo::ai_job::RepoTab::ai_free` (3), `repo::ai_job::RepoTab::cancel_ai` (2), `repo::ai_job::RepoTab::poll_ai` (4), `repo::ai_job::RepoTab::start_ai` (3), `repo::message::split` (2), `ui::inspector::wip::RepoTab::wip_panel` (31)

#### First commit guide (empty repo) `commit.first-commit-guide`
- **Entry:** empty-graph guidance panel in a repo without commits
- **Platform:** `platform.op-runner`
- **Shared:** `ui::graph_view::empty::first_commit_guide` (2)

### WIP panel extras

#### Conflicted files in the WIP panel `commit.wip-conflicts`
- **Entry:** "Conflicts N" list with "Mark all resolved"; row click opens the merge editor; row menu: Resolve with AI, Mark resolved, Use current/incoming, external merge tool, Restore conflict markers; folder menu "Mark folder resolved"
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `git::read::conflict_kind` (5), `repo::merge_editor::gitlinks` (5), `repo::merge_editor::stage_many` (2), `repo::merge_editor::take_side` (3), `repo::tools::mergetool_op` (3), `repo::tools::restore_markers_op` (3), `ui::inspector::file_row` (6), `ui::inspector::hovering` (5), `ui::inspector::row_button` (5), `ui::inspector::tree::Order::place` (7), `ui::inspector::tree::dir_row` (8), `ui::inspector::tree::indent` (7), `ui::inspector::tree::list` (7), `ui::inspector::tree::show` (7), `ui::inspector::tree::toggle` (7), `ui::inspector::tree::visible` (9), `ui::inspector::wip::RepoTab::wip_panel` (31), `ui::inspector::wip::SelCache::get` (4), `ui::inspector::wip::SelCache::has` (4), `ui::inspector::wip::mark_resolved_op` (2), `ui::inspector::wip::open_tools` (5), `ui::inspector::wip::pick_file` (4), `ui::inspector::wip::selected` (4), `ui::inspector::wip::shown` (2)

#### Operation-in-progress banner (merge / rebase / cherry-pick / revert / am) with Continue, Skip, Abort `commit.sequencer-banner`
- **Entry:** banner at top of the WIP panel (wip.rs:290)
- **Platform:** `platform.op-runner`, `platform.repo-load`
- **Shared:** `repo::op::continue_op` (2), `repo::op::sequencer_op` (2), `ui::inspector::wip::RepoTab::wip_panel` (31), `ui::inspector::wip::open_tools` (5)

#### Guided split Continue / Abort banner `commit.split-banner`
- **Entry:** banner above the inspector during a commit split (wip.rs:261)
- **Platform:** `platform.op-runner`
- **Shared:** `ui::inspector::wip::RepoTab::split_banner` (2)

#### Stash selected files / a folder `commit.stash-files`
- **Entry:** "Stash this file" / "Stash N files" in WIP row and folder menus (wip.rs stash_files_item)
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::op::stash_paths_op` (2), `ui::inspector::wip::stash_files_item` (3)

#### Add to .gitignore `commit.add-gitignore`
- **Entry:** untracked row menu and untracked folder menu "Add to .gitignore"
- **Platform:** `platform.watcher`
- **Own code:** `repo::files::append_gitignore`, `ui::inspector::wip::gitignore_item`

#### Show in file manager / copy path from WIP rows `commit.reveal-file`
- **Entry:** row and folder menus "Show in file manager" (Action::Reveal) and "Copy path"
- **Shared:** `repo::files::reveal` (3), `ui::inspector::wip::dir_items` (3)

#### Other WIP row menu items (open, history, blame, permalink, LFS track) `commit.wip-file-menu`
- **Entry:** row context menus in Unstaged / Staged lists
- **Platform:** `platform.op-runner`
- **Shared:** `ui::inspector::custom_items` (3), `ui::inspector::file_items` (2), `ui::inspector::lfs_track_item` (3), `ui::inspector::open_items` (4), `ui::inspector::permalink_items` (3), `ui::inspector::wip::dir_items` (3)

#### Absorb button (entry to Absorb) `commit.absorb-entry`
- **Entry:** "Absorb" button in the commit box header (wip.rs:332); absorb logic lives in another area
- **Shared:** `ui::inspector::wip::RepoTab::wip_panel` (31)

### Editing past commit messages

#### Edit a commit message in the inspector `commit.reword-message`
- **Entry:** click the message in commit details (commit.rs:212) -> inline editor, Save / ⌘↩, Cancel / Esc
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::msgedit::MsgEdit::changed` (2), `repo::msgedit::MsgEdit::message` (2), `repo::msgedit::MsgEdit::new` (2), `repo::msgedit::RepoData::follow` (3), `repo::msgedit::RepoData::path_from` (3), `repo::msgedit::RepoData::reword_path` (3), `repo::msgedit::RepoTab::mark_saved` (3), `repo::msgedit::RepoTab::sync_msg_edit` (3), `ui::inspector::commit::editable` (3), `ui::inspector::commit::msg_editor` (4)

#### Remove a co-author from a commit `commit.remove-coauthor`
- **Entry:** x next to "co-authored by" in commit details, or right-click "Remove co-author" (commit.rs:275)
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Own code:** `git::read::coauthor_line`, `git::read::parse_coauthors`, `repo::message::remove_coauthor`
- **Shared:** `repo::msgedit::MsgEdit::quiet` (2), `repo::msgedit::RepoData::follow` (3), `repo::msgedit::RepoData::path_from` (3), `repo::msgedit::RepoData::reword_path` (3), `repo::msgedit::RepoTab::mark_saved` (3), `repo::msgedit::RepoTab::sync_msg_edit` (3)

### Restore

#### Restore a file from a commit into the working tree `commit.restore-file`
- **Entry:** commit file context menu "Restore this file from this commit" / "Restore as it was before this commit" (commit.rs restore_items)
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::restore::restore_file` (3), `ui::inspector::commit::restore_items` (3)

<a id="refs"></a>
## Branches, tags and the sidebar

### Sidebar branch, remote and tag lists

#### Sidebar LOCAL / REMOTE / TAGS sections `refs.sidebar-lists`
- **Entry:** src/ui/sidebar/mod.rs:210 sidebar (left panel: LOCAL, REMOTE, TAGS collapsing sections with counts); src/ui/sidebar/mod.rs:201 section
- **Platform:** `platform.design-system`, `platform.git-model`, `platform.repo-load`
- **Own code:** `ui::sidebar::in_view`, `ui::sidebar::row`, `ui::sidebar::section`
- **Shared:** `git::read::refs` (2), `git::url::redact` (6), `repo::data::RepoData::branch_rank` (3), `repo::data::RepoData::local` (7), `repo::data::RepoData::split_remote` (13), `ui::inspector::tree::visible` (9), `ui::sidebar::head_row` (3), `ui::sidebar::local_row` (7), `ui::sidebar::pinned_separator` (2), `ui::sidebar::remote_row` (6), `ui::sidebar::sidebar` (5), `ui::sidebar::tag_row` (2)

#### Filter branches, remotes and tags `refs.sidebar-filter`
- **Entry:** src/ui/sidebar/mod.rs:222 filter text box "Filter" (top of sidebar)
- **Platform:** `platform.design-system`
- **Own code:** `ui::sidebar::matches`
- **Shared:** `ui::sidebar::sidebar` (5)

#### Branch and tag folders (feature/x grouping) `refs.sidebar-folders`
- **Entry:** src/ui/sidebar/folders.rs:29 show (folder rows in LOCAL, per-remote and TAGS lists; click to open/close; all open while filtering)
- **Platform:** `platform.design-system`
- **Own code:** `ui::sidebar::folders::ancestors`, `ui::sidebar::folders::show`
- **Shared:** `repo::filetree::FileTree::build_by` (2), `repo::filetree::FileTree::visible` (2), `repo::filetree::name_of` (2), `ui::inspector::tree::dir_row` (8), `ui::inspector::tree::visible` (9), `ui::sidebar::eye` (2)

#### Default / long-lived branches pinned on top `refs.pinned-branches`
- **Entry:** src/ui/sidebar/mod.rs:58 pinned_separator (thin line under main/develop etc.)
- **Shared:** `repo::data::RepoData::branch_rank` (3), `repo::data::RepoData::default_branch` (4), `ui::sidebar::pinned_separator` (2), `ui::sidebar::sidebar` (5)

#### Stacked branches nested under their parent `refs.stacked-display`
- **Entry:** src/ui/sidebar/mod.rs:88 stacked (indented rows, stack / stale icons, tooltip)
- **Shared:** `repo::stack::children` (5), `repo::stack::parent` (7), `repo::stack::stale` (6), `ui::sidebar::local_row` (7), `ui::sidebar::stacked` (2), `ui::sidebar::stacked::add` (2)

#### Ahead/behind, gone upstream and HEAD markers on branch rows `refs.sidebar-badges`
- **Entry:** src/ui/sidebar/mod.rs:113 local_row: check = HEAD, tree = checked out in another worktree (tooltip names it; double-click switches there), then the branch icon; stack / stale icons, "↑ n" / "↓ n" counts, tooltip "Upstream branch is gone"; detached HEAD row in sidebar (sidebar/mod.rs:237)
- **Platform:** `platform.git-model`
- **Shared:** `ui::sidebar::head_row` (3), `ui::sidebar::local_row` (7), `ui::sidebar::sidebar` (5)

#### Click a ref to jump to its commit; double-click to check out `refs.sidebar-jump`
- **Entry:** src/ui/sidebar/mod.rs:113 local_row / remote_row / tag_row click -> Action::Jump, double-click -> branch_checkout
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::op::branch_checkout` (7), `ui::sidebar::local_row` (7), `ui::sidebar::remote_row` (6), `ui::sidebar::tag_row` (2)

### Checkout

#### Check out a branch, remote branch or tag `refs.checkout`
- **Entry:** src/ui/sidebar/ref_menus.rs:181 "Checkout <name>" (branch/remote/tag menus, sidebar and graph pills); double-click sidebar row (sidebar/mod.rs:113 local_row) or graph pill (graph_view/mod.rs:458 act closure); graph commit menu "Checkout <ref>" and double-click commit row (graph_view/menus.rs:94 checkout_action, :141); palette "Checkout <ref>" (src/ui/palette.rs:198)
- **Platform:** `platform.git-runner`, `platform.offer`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::data::RepoData::local` (7), `repo::data::RepoData::split_remote` (13), `repo::op::Op::new` (7), `repo::op::Op::undo` (4), `repo::op::branch_checkout` (7), `repo::op::checkout_args` (3), `repo::op::in_progress` (4), `repo::op::switch_refused` (3), `repo::op::switches_head` (3), `ui::graph_view::menus::checkout_action` (2), `ui::graph_view::menus::commit_menu` (24), `ui::graph_view::show` (17), `ui::palette::items` (25), `ui::sidebar::ref_menus::local_menu` (22), `ui::sidebar::ref_menus::remote_menu` (9), `ui::sidebar::ref_menus::tag_menu` (11)

#### Checkout of a remote branch creates / fast-forwards a tracking branch `refs.checkout-remote-tracking`
- **Entry:** same entries as refs.checkout on a remote branch row/pill: update local twin, new tracking branch, or prompt for a new name when twin tracks elsewhere
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::action::prompt` (7), `repo::data::RepoData::local` (7), `repo::data::RepoData::split_remote` (13), `repo::op::branch_checkout` (7), `repo::op::checkout_args` (3), `ui::dialogs::prompt_dialog` (8), `ui::sidebar::ref_menus::remote_menu` (9), `ui::sidebar::remote_row` (6)

#### Checking out a branch held by another worktree opens that worktree `refs.checkout-worktree-switch`
- **Entry:** Checkout / double-click on a branch with a worktree marker (op.rs:1051 branch_checkout -> Action::Switch)
- **Platform:** `platform.op-runner`
- **Shared:** `repo::op::branch_checkout` (7)

#### Checkout this commit (detached) `refs.checkout-detached`
- **Entry:** src/ui/graph_view/menus.rs:151 "Checkout this commit (detached)"; tag checkout (detached) via refs.checkout
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::op::branch_checkout` (7), `ui::graph_view::menus::commit_menu` (24)

#### Checkout blocked by local changes: stash, switch, reapply `refs.checkout-autostash`
- **Entry:** failure dialog "Uncommitted changes in the way" -> "Stash, switch and reapply" (src/repo/offer.rs:36)
- **Platform:** `platform.offer`, `platform.op-runner`
- **Shared:** `repo::offer::autostash_checkout` (3), `repo::offer::offer` (2), `repo::op::exec_stash` (11)

#### Refuse branch switch during merge/rebase/etc. `refs.checkout-refused-midop`
- **Entry:** any checkout while an operation is in progress (op.rs:1104 switch_refused)
- **Platform:** `platform.op-runner`
- **Shared:** `repo::op::in_progress` (4), `repo::op::switch_refused` (3), `repo::op::switches_head` (3)

### Branch management

#### Create branch (from here, from commit, from HEAD) `refs.create-branch`
- **Entry:** src/ui/sidebar/ref_menus.rs:230 local_menu "Create branch from here…"; src/ui/graph_view/menus.rs:170 "Create branch here…" (commit menu); src/ui/toolbar.rs:287 toolbar_local branch button ("New branch from HEAD"); palette "Create branch…" (src/ui/palette.rs:93)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::action::prompt` (7), `repo::op::Op::new` (7), `repo::op::Op::undo` (4), `ui::dialogs::fill` (8), `ui::dialogs::fill_arg` (7), `ui::dialogs::prompt_dialog` (8), `ui::graph_view::menus::commit_menu` (24), `ui::palette::items` (25), `ui::sidebar::ref_menus::local_menu` (22), `ui::toolbar::toolbar_local` (5)

#### Rename branch `refs.rename-branch`
- **Entry:** src/ui/sidebar/ref_menus.rs:233 local_menu "Rename…"
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::action::prompt` (7), `repo::op::Op::new` (7), `repo::op::Op::revert` (5), `ui::dialogs::fill` (8), `ui::dialogs::fill_arg` (7), `ui::dialogs::prompt_dialog` (8), `ui::sidebar::ref_menus::local_menu` (22)

#### Set upstream of a branch `refs.set-upstream`
- **Entry:** src/ui/sidebar/ref_menus.rs:241 local_menu "Set upstream…"
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::action::prompt` (7), `repo::data::RepoData::default_remote` (8), `repo::op::Op::new` (7), `ui::dialogs::fill` (8), `ui::dialogs::fill_arg` (7), `ui::dialogs::prompt_dialog` (8), `ui::sidebar::ref_menus::local_menu` (22)

#### Unset upstream of a branch `refs.unset-upstream`
- **Entry:** src/ui/sidebar/ref_menus.rs:244 local_menu "Unset upstream (<up>)"
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `ui::sidebar::ref_menus::local_menu` (22)

#### Delete / force delete a local branch (undoable) `refs.delete-branch`
- **Entry:** src/ui/sidebar/ref_menus.rs:43 delete_items "Delete" / :46 "Force delete" (last in the branch menu behind a separator; sidebar row and graph pill)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::data::RepoData::local` (7), `repo::op::Op::confirm` (4), `repo::op::Op::revert` (5), `repo::op::branch_cmd` (3), `repo::op::delete_branches` (4), `repo::op::keep_deleted` (2), `ui::sidebar::ref_menus::delete_items` (8), `ui::sidebar::ref_menus::ref_menu` (4), `ui::sidebar::ref_menus::synced_menu` (5)

#### Delete a branch from the remote `refs.delete-remote-branch`
- **Entry:** src/ui/sidebar/ref_menus.rs:60 delete_items "Delete <branch> from <remote>" (last in the remote branch menu; on a synced graph pill after the local branch's delete)
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::data::RepoData::split_remote` (13), `repo::remotes::delete_branch` (3), `ui::sidebar::ref_menus::delete_items` (8), `ui::sidebar::ref_menus::synced_menu` (5)

#### Fast-forward a non-checked-out branch to its upstream `refs.fast-forward-branch`
- **Entry:** src/ui/sidebar/ref_menus.rs:185 local_menu "Fast-forward <name> to <upstream>"
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::op::Op::new` (7), `repo::op::Op::undo` (4), `ui::sidebar::ref_menus::local_menu` (22)

#### Pull a branch (fast-forward) without checking it out `refs.pull-without-checkout`
- **Entry:** src/ui/sidebar/ref_menus.rs:188 local_menu "Pull <name> (fast-forward, without checkout)"
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::data::RepoData::split_remote` (13), `repo::remotes::pull_ff` (2), `ui::sidebar::ref_menus::local_menu` (22)

#### Push a branch to its remote from the menu `refs.push-branch`
- **Entry:** src/ui/sidebar/ref_menus.rs:195 local_menu "Push <name> to <remote>"
- **Platform:** `platform.git-runner`, `platform.offer`, `platform.op-runner`
- **Shared:** `repo::data::RepoData::push_target` (4), `repo::data::RepoData::split_remote` (13), `ui::sidebar::ref_menus::local_menu` (22)

#### Move unpushed commits to a new branch (HEAD menu) `refs.move-unpushed-to-branch`
- **Entry:** src/ui/sidebar/ref_menus.rs:248 local_menu on HEAD branch (git-fork item)
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::spinoff::move_unpushed` (2), `ui::sidebar::ref_menus::local_menu` (22)

#### Open a branch in a new worktree / open its worktree `refs.branch-worktree`
- **Entry:** src/ui/sidebar/ref_menus.rs:271 worktree_items "Open in a new worktree…", :276 "Open its worktree"; remote menu :308 "Check out in a new worktree…"
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::action::prompt` (7), `repo::data::RepoData::worktree_path` (3), `repo::data::RepoData::worktree_rev` (2), `repo::op::Op::then_open` (4), `ui::dialogs::fill` (8), `ui::dialogs::fill_arg` (7), `ui::dialogs::prompt_dialog` (8), `ui::sidebar::ref_menus::remote_menu` (9), `ui::sidebar::ref_menus::worktree_items` (6)

#### Merge / rebase / interactive rebase from a ref menu `refs.merge-rebase-from-menu`
- **Entry:** src/ui/sidebar/ref_menus.rs:224 local_menu and :316 remote_menu via integrate_items ("Merge <ref> into <head>", with options, "Rebase <head> onto <ref>", "Interactive rebase…")
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::merge_opts::Merge::name` (4), `repo::merge_opts::merge_op` (4), `ui::graph_view::menus::integrate_items` (6), `ui::sidebar::ref_menus::local_menu` (22), `ui::sidebar::ref_menus::remote_menu` (9)

#### Reset current branch to a ref (soft/mixed/keep/hard) `refs.reset-to-ref`
- **Entry:** ref menus "Reset <head> to <ref>" (graph_view/menus.rs:305 reset_menu) in local, remote and tag menus
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::op::reset_op`
- **Shared:** `ui::graph_view::menus::reset_busy` (2), `ui::graph_view::menus::reset_items` (2), `ui::graph_view::menus::reset_menu` (2), `ui::sidebar::ref_menus::local_menu` (22), `ui::sidebar::ref_menus::remote_menu` (9), `ui::sidebar::ref_menus::tag_menu` (11)

#### Copy branch / tag name `refs.copy-ref-name`
- **Entry:** ref menus "Copy branch name" / "Copy tag name"
- **Shared:** `ui::sidebar::ref_menus::local_menu` (22), `ui::sidebar::ref_menus::remote_menu` (9), `ui::sidebar::ref_menus::tag_menu` (11)

#### Right-click menu for refs (sidebar rows and graph pills) in one shared order `refs.ref-menu-dispatch`
- **Entry:** src/ui/sidebar/ref_menus.rs:12 ref_menu; :25 synced_menu for a pill with local + remote at the same commit (the local's items, then the remote's own, then both deletes); order in every ref menu: switch and sync (checkout, fast-forward, pull, push, stack) / into the checked-out branch (merge, rebase, reset, compare) / the branch itself (create, rename, upstream, move unpushed) / worktree items / web links / copy name / hide and solo (view_items) / destructive items last behind a separator (:38 delete_items); the commit, WIP-file, folder, stash and remote menus also end with their discard / delete / remove item
- **Shared:** `ui::sidebar::ref_menus::local_menu` (22), `ui::sidebar::ref_menus::ref_menu` (4), `ui::sidebar::ref_menus::remote_menu` (9), `ui::sidebar::ref_menus::synced_menu` (5), `ui::sidebar::ref_menus::tag_menu` (11), `ui::sidebar::ref_menus::view_items` (3), `ui::sidebar::ref_menus::delete_items` (8), `ui::sidebar::ref_menus::worktree_items` (6)

#### Restack / push stack from a branch menu `refs.stack-menu`
- **Entry:** src/ui/sidebar/ref_menus.rs:206 local_menu "Restack", "Push stack (n branches)"
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::stack::members` (5), `repo::stack::push_op` (3), `repo::stack::restack_op` (2), `repo::stack::stale` (6), `ui::sidebar::ref_menus::local_menu` (22)

#### Delete a branch with its remote branch and worktree in one step `refs.delete-everywhere`
- **Entry:** src/ui/sidebar/ref_menus.rs:51 delete_items "Delete <branch> and <remote>/<branch> and its worktree" (last in the branch menu; sidebar row and graph pill)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::op::delete_everywhere`
- **Shared:** `repo::data::RepoData::local` (7), `repo::data::RepoData::split_remote` (13), `repo::op::Op::confirm` (4), `repo::op::Op::revert` (5), `repo::op::branch_cmd` (3), `repo::op::delete_branches` (4), `repo::op::keep_deleted` (2), `repo::op::remove_worktree_and_branch` (2), `repo::op::worktree_remove_args` (4), `repo::remotes::delete_branch` (3), `ui::sidebar::ref_menus::delete_items` (8)

#### Remove a branch's worktree and keep the branch `refs.remove-worktree-keep-branch`
- **Entry:** src/ui/sidebar/ref_menus.rs:281 worktree_items "Remove its worktree (keep <branch>)" (branch checked out in another unlocked linked worktree; sidebar row and graph pill); src/ui/sidebar/worktrees.rs "Remove worktree" on the worktree row
- **Platform:** `platform.op-runner`, `platform.git-runner`
- **Shared:** `repo::op::remove_worktree_op` (3), `repo::op::removes_worktree` (3), `repo::op::worktree_remove` (3), `repo::op::worktree_path` (3), `repo::op::worktree_remove_args` (4), `ui::sidebar::ref_menus::worktree_items` (6)

### Compare and web links from ref menus

#### Compare ref with working tree / with current branch `refs.compare-from-menu`
- **Entry:** src/ui/sidebar/ref_menus.rs:102 "Compare working tree with <ref>" and "Compare with <head>" (local, remote, tag menus)
- **Own code:** `ui::sidebar::ref_menus::worktree_compare`
- **Shared:** `repo::data::RepoData::local` (7), `repo::data::RepoData::rev` (3), `ui::sidebar::ref_menus::compare_items` (3), `ui::sidebar::ref_menus::local_menu` (22), `ui::sidebar::ref_menus::remote_menu` (9), `ui::sidebar::ref_menus::tag_menu` (11)

#### Explain changes of a branch vs current (AI) `refs.explain-branch-ai`
- **Entry:** src/ui/sidebar/ref_menus.rs:115 compare_items "Explain changes vs <head>"
- **Platform:** `platform.ai-client`
- **Shared:** `ui::sidebar::ref_menus::compare_items` (3)

#### Create pull request / Write pull request (AI) / Open branch on remote `refs.pr-from-menu`
- **Entry:** src/ui/sidebar/ref_menus.rs:167 web_items "Create pull request…", "Write pull request…", "Open branch on remote"; palette "Create pull request for <branch>" (palette.rs)
- **Platform:** `platform.ai-client`
- **Shared:** `git::short_ref` (2), `git::url::Page::url` (4), `git::url::url_encode` (3), `git::url::url_path` (3), `repo::data::RepoData::default_branch` (4), `repo::data::RepoData::web_of` (4), `repo::stack::parent` (7), `ui::sidebar::ref_menus::pr_base` (2), `ui::sidebar::ref_menus::pull_request_url` (2), `ui::sidebar::ref_menus::remote_branch` (2), `ui::sidebar::ref_menus::web_items` (3)

#### Open compare page on the remote `refs.compare-link`
- **Entry:** src/ui/sidebar/ref_menus.rs:121 compare_items (links::items "compare")
- **Shared:** `git::url::Page::url` (4), `ui::links::items` (3), `ui::sidebar::ref_menus::compare_items` (3)

#### Open release page of a tag `refs.release-link`
- **Entry:** src/ui/sidebar/ref_menus.rs:344 tag_menu (links::items "release page")
- **Shared:** `git::url::Page::url` (4), `ui::links::items` (3), `ui::sidebar::ref_menus::tag_menu` (11)

### Tags

#### Create tag (lightweight) `refs.create-tag`
- **Entry:** src/ui/graph_view/menus.rs:184 "Create tag here…" (commit menu)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::action::prompt` (7), `ui::dialogs::fill` (8), `ui::dialogs::fill_arg` (7), `ui::dialogs::prompt_dialog` (8), `ui::graph_view::menus::commit_menu` (24)

#### Create annotated / signed tag `refs.create-annotated-tag`
- **Entry:** src/ui/graph_view/menus.rs:187 "Create annotated tag here…" (message box + "Sign tag" checkbox in prompt dialog)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::data::RepoData::cfg_bool` (2), `repo::op::signed_tag` (2), `ui::dialogs::fill` (8), `ui::dialogs::fill_arg` (7), `ui::dialogs::prompt_dialog` (8), `ui::graph_view::menus::commit_menu` (24)

#### Signing failed: offer to tag unsigned `refs.tag-signing-fallback`
- **Entry:** failure dialog from tag --sign (offer.rs:58)
- **Platform:** `platform.offer`
- **Shared:** `repo::offer::signing_error` (2), `repo::offer::signing_failed` (2), `repo::op::signed_tag` (2)

#### Move tag here (keeps annotation) `refs.move-tag`
- **Entry:** src/ui/graph_view/menus.rs:72 "Move tag here" submenu in commit menu
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::tags::move_tag`, `repo::tags::tag_message`
- **Shared:** `ui::graph_view::menus::commit_menu` (24), `ui::graph_view::menus::move_tag_menu` (2)

#### Delete tag (local) `refs.delete-tag`
- **Entry:** src/ui/sidebar/ref_menus.rs:67 delete_items "Delete tag" (last in the tag menu)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `ui::sidebar::ref_menus::delete_items` (8)

#### Delete tag from remote `refs.delete-tag-remote`
- **Entry:** src/ui/sidebar/ref_menus.rs:71 delete_items "Delete tag from <remote>"
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::data::RepoData::default_remote` (8), `ui::sidebar::ref_menus::delete_items` (8)

#### Delete tag here and from remote `refs.delete-tag-everywhere`
- **Entry:** src/ui/sidebar/ref_menus.rs:74 delete_items "Delete tag here and from <remote>"
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::tags::delete_everywhere`
- **Shared:** `repo::data::RepoData::default_remote` (8), `ui::sidebar::ref_menus::delete_items` (8)

#### Push a tag `refs.push-tag`
- **Entry:** src/ui/sidebar/ref_menus.rs:331 tag_menu "Push tag to <remote>"
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::data::RepoData::default_remote` (8), `ui::sidebar::ref_menus::tag_menu` (11)

#### Push all tags `refs.push-all-tags`
- **Entry:** src/ui/sidebar/ref_menus.rs:334 tag_menu "Push all tags to <remote>"; src/ui/toolbar.rs:187 history_tools menu "Push all tags to <remote>"; palette "Push all tags to <remote>" (palette.rs:170)
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::tags::push_all` (2), `ui::palette::items` (25), `ui::sidebar::ref_menus::tag_menu` (11), `ui::toolbar::history_tools` (7)

#### Verify tag signature `refs.verify-tag`
- **Entry:** src/ui/sidebar/ref_menus.rs:341 tag_menu "Verify signature" (annotated tags) -> Action::VerifyTag (repo/mod.rs:720)
- **Platform:** `platform.task`
- **Own code:** `git::read::verify_tag`
- **Shared:** `repo::RepoTab::apply` (21), `ui::sidebar::ref_menus::tag_menu` (11)

### Graph visibility: hide, solo, hover

#### Hide / show a ref in the graph `refs.hide-from-graph`
- **Entry:** src/ui/sidebar/mod.rs:52 eye icon on hovered rows and folder/remote headers ("Hide from graph"); ref menus "Hide from graph" / "Show in graph" (ref_menus.rs:89 view_items); persisted per repo in ui/repo_view.rs:137
- **Platform:** `platform.op-runner`, `platform.repo-load`
- **Shared:** `repo::RepoTab::apply` (21), `repo::RepoTab::reload` (3), `repo::data::RepoData::shows` (2), `repo::data::View::change` (3), `repo::data::View::revs` (3), `ui::repo_view::show` (6), `ui::sidebar::eye` (2), `ui::sidebar::ref_menus::view_items` (3), `ui::sidebar::view_style` (2)

#### Solo a ref (show only soloed refs) `refs.solo-ref`
- **Entry:** ref menus "Solo (show only soloed refs)" / "Unsolo" (ref_menus.rs:95 view_items); soloed rows orange in sidebar (sidebar/mod.rs:26 view_style)
- **Platform:** `platform.repo-load`
- **Shared:** `repo::RepoTab::apply` (21), `repo::RepoTab::reload` (3), `repo::data::View::change` (3), `repo::data::View::revs` (3), `ui::repo_view::show` (6), `ui::sidebar::ref_menus::view_items` (3), `ui::sidebar::view_style` (2)

#### Ref hover highlight (fade commits a pill can't reach) `refs.hover-highlight`
- **Entry:** hover a graph ref pill for 50ms (src/ui/graph_view/mod.rs:459 act closure -> hover.rs:9 hover_ref; graph_view show reads hovered_ref)
- **Own code:** `ui::graph_view::hover::hover_ref`, `ui::graph_view::hover::hovered_ref`
- **Shared:** `graph::layout::Graph::has_unloaded_parent` (2), `graph::layout::Graph::reachable` (2), `repo::data::RepoData::reachable` (3), `ui::graph_view::show` (17)

#### Ref pills in the graph (icons, names cut with …, +N badge, tooltip, drag source) `refs.graph-pills`
- **Entry:** src/ui/graph_view/mod.rs:71 ref_pill, :53 ref_label (check = HEAD, tree = checked out in another worktree, cloud = upstream at the same commit, then the branch icon; long names cut with …), :112 pill_tip (upstream, worktree path and how to switch there, tag message); click selects, double-click checks out, right-click ref menu
- **Shared:** `repo::op::branch_checkout` (7), `ui::graph_view::pill_tip` (3), `ui::graph_view::ref_label` (3), `ui::graph_view::ref_pill` (4), `ui::graph_view::show` (17), `ui::graph_view::tip_now` (3), `ui::sidebar::ref_menus::ref_menu` (4), `ui::sidebar::ref_menus::synced_menu` (5)

#### HEAD pill: filled with its lane colour, glowing, leads with a check `refs.head-pill`
- **Entry:** graph ref column (src/ui/graph_view/mod.rs:71 ref_pill): the checked-out branch's pill is filled with its lane colour behind a soft glow, with bold text and its connector line to the node in the same colour; HEAD's node is larger; label (src/ui/graph_view/mod.rs:53 ref_label): check, then a cloud when its upstream is at the same commit, then the branch icon and name; cut with … when the column is narrow; sidebar: HEAD's row is a solid capsule (src/ui/sidebar/mod.rs:79 head_row)
- **Platform:** `platform.design-system`
- **Shared:** `ui::graph_view::ref_pill` (4), `ui::graph_view::ref_label` (3), `ui::graph_view::show` (17), `ui::sidebar::head_row` (3), `ui::sidebar::local_row` (7)

### Drag and drop branches

#### Drag a branch onto another: merge / push / rebase menu `refs.drag-branch-onto-branch`
- **Entry:** sidebar branch/remote rows (sidebar/mod.rs branch_dnd), graph pills (graph_view/mod.rs act closure), graph commit rows (graph_view/mod.rs:329) -> Action::DropBranch -> dialogs.rs:472 drop_menu
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::RepoTab::apply` (21), `repo::data::RepoData::rev` (3), `repo::data::RepoData::split_remote` (13), `repo::merge_opts::Merge::name` (4), `repo::merge_opts::merge_op` (4), `repo::op::checkout_args` (3), `ui::dialogs::drop_menu` (5), `ui::graph_view::dnd::branch_dnd` (3), `ui::graph_view::show` (17), `ui::sidebar::local_row` (7), `ui::sidebar::remote_row` (6)

#### Drag preview label following the pointer `refs.drag-preview`
- **Entry:** src/ui/app.rs:592 graph_view::dnd::preview
- **Platform:** `platform.design-system`
- **Shared:** `ui::graph_view::dnd::preview` (3)

#### Drop a commit onto a branch row/pill (cherry-pick menu) `refs.drop-commit-on-branch`
- **Entry:** branch_dnd CommitDrag release (graph_view/dnd.rs:16) -> Action::DropCommit -> commit_drop_menu
- **Platform:** `platform.task`
- **Shared:** `repo::merge_opts::pick_op` (5), `repo::op::RepoTab::drop_commit` (3), `repo::op::RepoTab::drop_menu` (4), `ui::dialogs::commit_drop_menu` (3), `ui::graph_view::dnd::branch_dnd` (3)

### Branch cleanup

#### Clean up branches dialog (merged, squashed, gone, stale) `refs.cleanup-open`
- **Entry:** src/ui/sidebar/mod.rs:219 broom button "Clean up merged & gone branches"; palette "Clean up branches (merged, squashed, gone, stale)…" (palette.rs:149); src/ui/dialogs.rs:242 cleanup_dialog
- **Platform:** `platform.format`, `platform.task`
- **Own code:** `repo::cleanup::Cleanup::group`, `repo::cleanup::Cleanup::rescan`, `repo::cleanup::source`
- **Shared:** `repo::cleanup::Cleanup::poll` (2), `repo::cleanup::RepoTab::open_cleanup` (4), `repo::cleanup::matches` (2), `repo::cleanup::protected` (2), `ui::dialogs::cleanup_dialog` (4), `ui::palette::items` (25), `ui::sidebar::sidebar` (5)

#### Scan branches: merged / squash- or rebase-merged / gone / stale `refs.cleanup-scan`
- **Entry:** background scan started by open_cleanup (repo/cleanup.rs:262 scan)
- **Platform:** `platform.git-runner`, `platform.task`
- **Own code:** `repo::cleanup::counts`, `repo::cleanup::same_tree`, `repo::cleanup::twins`, `repo::cleanup::unpushed`
- **Shared:** `git::network::fetch_remote_quiet` (2), `repo::cleanup::Cleanup::poll` (2), `repo::cleanup::RepoTab::open_cleanup` (4), `repo::cleanup::scan` (2)

#### Protected branches never offered (Settings keep_branches, default branch, target) `refs.cleanup-protected`
- **Entry:** Settings "hotfix/*, staging" box; cleanup.rs:58 protected
- **Shared:** `repo::cleanup::RepoTab::open_cleanup` (4), `repo::cleanup::matches` (2), `repo::cleanup::protected` (2)

#### Also delete merged branches on the remote `refs.cleanup-delete-remote`
- **Entry:** cleanup dialog checkbox "Also delete on the remote"
- **Platform:** `platform.git-runner`
- **Shared:** `git::network::fetch_remote_quiet` (2), `repo::cleanup::Cleanup::remote_deletes` (2), `repo::cleanup::remote_delete` (2), `repo::cleanup::scan` (2), `ui::dialogs::cleanup_dialog` (4)

#### Prune stale remote-tracking refs `refs.cleanup-prune`
- **Entry:** cleanup dialog checkbox "Prune stale remote-tracking refs"
- **Platform:** `platform.git-runner`
- **Own code:** `repo::cleanup::stale_refs`
- **Shared:** `repo::cleanup::Cleanup::pruned` (2), `repo::cleanup::RepoTab::open_cleanup` (4), `ui::dialogs::cleanup_dialog` (4)

#### Delete the ticked branches (undoable) `refs.cleanup-run`
- **Entry:** cleanup dialog primary button "Delete n branches"
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::cleanup::Cleanup::chosen`, `repo::cleanup::Cleanup::op`
- **Shared:** `repo::cleanup::Cleanup::pruned` (2), `repo::cleanup::Cleanup::remote_deletes` (2), `repo::cleanup::remote_delete` (2), `repo::op::delete_branches` (4), `ui::dialogs::cleanup_dialog` (4)

<a id="rewrite"></a>
## History rewriting and stacks

### Interactive rebase editor

#### Interactive rebase editor (reorder, pick/reword/squash/fixup/drop/edit) `rewrite.rebase-editor`
- **Entry:** src/ui/graph_view/menus.rs:262 "Edit history after this commit (interactive rebase)…" -> Action::Rebase; src/ui/graph_view/menus.rs:127 "Interactive rebase {head} onto {target}…" (integrate_items; branch/ref context menus); src/ui/dialogs.rs:524 branch drop menu "Interactive rebase {src} onto {dst}…"; keys in editor: ↑/↓ select, ⌥↑/⌥↓ move, P R S F D, Esc cancel (src/ui/rebase_editor.rs:24); buttons: move up/down pills, action pills, "Start rebase", "Cancel" (src/ui/rebase_editor.rs); src/ui/repo_view.rs:87 center-pane switch
- **Platform:** `platform.design-system`, `platform.git-runner`, `platform.offer`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `ui::rebase_editor::field`
- **Shared:** `repo::RepoTab::apply` (21), `repo::RepoTab::prepare` (5), `repo::RepoTab::unpark_rebase` (2), `repo::rebase::Editor::edited` (2), `repo::rebase::Editor::load` (6), `repo::rebase::Editor::op` (6), `repo::rebase::Editor::problem` (4), `repo::rebase::Editor::start` (2), `repo::rebase::head_state` (12), `repo::rebase::items` (7), `repo::rebase::no_hooks` (3), `repo::rebase::sh_quote` (6), `repo::rebase::stacked_refs` (7), `ui::dialogs::drop_menu` (5), `ui::graph_view::menus::commit_menu` (24), `ui::graph_view::menus::integrate_items` (6), `ui::graph_view::menus::on_branch` (3), `ui::rebase_editor::show` (2), `ui::repo_view::show` (6)

#### Warn before rewriting pushed commits `rewrite.rebase-confirm-pushed`
- **Entry:** any rebase / reword / squash / drop / split / amend-into op: confirm dialog (src/ui/dialogs.rs confirm_dialog) built by rebase::confirm_*; Action::RunRebase (src/repo/mod.rs:847)
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::rebase::confirm_pushed` (7), `repo::rebase::confirm_rebase` (2), `repo::rebase::confirm_revs` (7), `repo::rebase::rewrites_pushed` (7)

#### Rebase onto a branch / commit (non-interactive) `rewrite.rebase-onto`
- **Entry:** src/ui/graph_view/menus.rs:123 "Rebase {head} onto {target}" (graph + sidebar branch menus via integrate_items); src/ui/dialogs.rs:520 branch drop menu "Rebase {src} onto {dst}"; ⌘P "Rebase {head} onto {ref}" (src/ui/palette.rs:206)
- **Platform:** `platform.git-runner`, `platform.offer`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::RepoTab::apply` (21), `repo::rebase::confirm_pushed` (7), `repo::rebase::confirm_rebase` (2), `repo::rebase::confirm_revs` (7), `repo::rebase::rewrites_pushed` (7), `ui::dialogs::drop_menu` (5), `ui::graph_view::menus::integrate_items` (6), `ui::palette::items` (25)

#### Interactive cherry-pick (multi-selection) `rewrite.interactive-cherry-pick`
- **Entry:** src/ui/graph_view/menus.rs:21 multi menu "Cherry-pick interactively…" -> MultiOp::CherryPickEdit; palette-less; multi-selection via cmd/shift click
- **Platform:** `platform.design-system`, `platform.git-runner`, `platform.op-runner`
- **Own code:** `repo::rebase::Editor::cherry_pick`
- **Shared:** `repo::op::RepoTab::multi_op` (4), `repo::rebase::Editor::edited` (2), `repo::rebase::Editor::op` (6), `repo::rebase::Editor::problem` (4), `repo::rebase::Editor::start` (2), `repo::rebase::head_state` (12), `repo::rebase::items` (7), `repo::rebase::sh_quote` (6), `ui::graph_view::menus::multi_menu` (7), `ui::graph_view::multi_select` (3), `ui::rebase_editor::show` (2)

#### Guard against a moved branch / stale plan `rewrite.rebase-guard`
- **Entry:** exec pseudo-commands HEAD_IS and REFS_AT at the head of rebase / amend / split / reword ops; Editor::current (unsaved-plan check, src/ui/app.rs close prompt)
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Own code:** `repo::rebase::Editor::current`, `repo::rebase::refs_at`
- **Shared:** `repo::rebase::head_state` (12)

### Quick commit rewrites

#### Edit a commit message (inline in details, reword) `rewrite.reword-inline`
- **Entry:** Commit details "Edit message…" / editor Save, ⌘Enter (src/ui/inspector/commit.rs:212, msg_editor); Graph commit menu → Rewrite this commit → "Edit message…" (menus.rs:276, via __reword__ prompt); Co-authored-by removal (commit.rs:299)
- **Platform:** `platform.git-model`, `platform.git-runner`, `platform.offer`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::RepoTab::apply` (21), `repo::RepoTab::prepare` (5), `repo::msgedit::MsgEdit::changed` (2), `repo::msgedit::MsgEdit::message` (2), `repo::msgedit::MsgEdit::new` (2), `repo::msgedit::MsgEdit::quiet` (2), `repo::msgedit::RepoData::follow` (3), `repo::msgedit::RepoData::path_from` (3), `repo::msgedit::RepoData::reword_path` (3), `repo::msgedit::RepoTab::mark_saved` (3), `repo::msgedit::RepoTab::sync_msg_edit` (3), `repo::op::RepoTab::run` (5), `repo::rebase::batch_object` (2), `repo::rebase::confirm_pushed` (7), `repo::rebase::confirm_revs` (7), `repo::rebase::head_state` (12), `repo::rebase::quick` (2), `repo::rebase::reword` (2), `repo::rebase::rewrite` (3), `repo::rebase::rewrites_pushed` (7), `ui::inspector::commit::editable` (3), `ui::inspector::commit::msg_editor` (4)

#### Rewrite commit without checkout (REWRITE path: merges / other branches) `rewrite.rewrite-no-checkout`
- **Entry:** Reword of a commit behind merges or on another local branch (Action::Reword -> rebase::reword fallback); exec pseudo-command !rewrite in op execution
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::rebase::batch_object` (2), `repo::rebase::reword` (2), `repo::rebase::rewrite` (3), `repo::rebase::stacked_refs` (7)

#### Squash into parent / Drop commit (graph menu) `rewrite.squash-drop-one`
- **Entry:** src/ui/graph_view/menus.rs:289 "Squash into parent"; src/ui/graph_view/menus.rs:293 "Drop commit"
- **Platform:** `platform.git-runner`, `platform.offer`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::RepoTab::apply` (21), `repo::RepoTab::prepare` (5), `repo::rebase::Editor::load` (6), `repo::rebase::Editor::op` (6), `repo::rebase::Editor::problem` (4), `repo::rebase::confirm_pushed` (7), `repo::rebase::confirm_revs` (7), `repo::rebase::head_state` (12), `repo::rebase::items` (7), `repo::rebase::quick` (2), `repo::rebase::rewrites_pushed` (7), `repo::rebase::sh_quote` (6), `repo::rebase::stacked_refs` (7), `ui::graph_view::menus::commit_menu` (24)

#### Squash / Drop several selected commits `rewrite.squash-drop-multi`
- **Entry:** src/ui/graph_view/menus.rs:39 "Squash {n} commits into one"; src/ui/graph_view/menus.rs:41 "Drop {n} commits"; multi-select panel buttons (src/ui/inspector/multi.rs:26)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::rebase::multi`
- **Shared:** `repo::op::RepoTab::multi_op` (4), `repo::rebase::Editor::load` (6), `repo::rebase::Editor::op` (6), `repo::rebase::confirm_pushed` (7), `repo::rebase::confirm_revs` (7), `repo::rebase::head_state` (12), `repo::rebase::items` (7), `repo::rebase::rewrites_pushed` (7), `repo::rebase::sh_quote` (6), `repo::rebase::stacked_refs` (7), `ui::graph_view::menus::multi_menu` (7), `ui::graph_view::multi_select` (3), `ui::inspector::multi::RepoTab::multi_panel` (4)

#### Amend staged changes into an older commit `rewrite.amend-into`
- **Entry:** src/ui/graph_view/menus.rs:269 Rewrite this commit → "Amend staged changes into this commit"
- **Platform:** `platform.git-runner`, `platform.offer`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::rebase::amend_into`
- **Shared:** `repo::RepoTab::apply` (21), `repo::RepoTab::prepare` (5), `repo::rebase::Editor::load` (6), `repo::rebase::confirm_pushed` (7), `repo::rebase::confirm_revs` (7), `repo::rebase::head_state` (12), `repo::rebase::items` (7), `repo::rebase::no_hooks` (3), `repo::rebase::pending_autosquash` (2), `repo::rebase::rewrites_pushed` (7), `ui::graph_view::menus::commit_menu` (24)

### Drag and drop in the graph

#### Drag a commit onto a commit: move / squash / fixup `rewrite.drag-reorder`
- **Entry:** drag a commit row onto another commit row (src/ui/graph_view/mod.rs:344 -> Action::DropCommit); ⌥ held squashes right away; popup menu: "Move above / below <commit>", "Squash into <commit>", "Fixup into <commit> (drop this message)" (built in src/repo/op.rs:1478 drop_menu, shown by src/ui/dialogs.rs commit_drop_menu)
- **Platform:** `platform.git-runner`, `platform.offer`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::rebase::drag`, `repo::rebase::drag_editor`, `repo::rebase::on_branch`
- **Shared:** `repo::RepoTab::apply` (21), `repo::op::RepoTab::drop_commit` (3), `repo::op::RepoTab::drop_menu` (4), `repo::rebase::Editor::load` (6), `repo::rebase::Editor::op` (6), `repo::rebase::Editor::problem` (4), `repo::rebase::head_state` (12), `repo::rebase::items` (7), `repo::rebase::sh_quote` (6), `repo::rebase::stacked_refs` (7), `ui::dialogs::commit_drop_menu` (3), `ui::graph_view::dnd::preview` (3), `ui::graph_view::show` (17)

#### Drop a commit onto a branch: cherry-pick `rewrite.drop-cherrypick-onto-branch`
- **Entry:** drag a commit onto a branch pill / sidebar row (src/ui/graph_view/dnd.rs:31 Action::DropCommit)
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::op::RepoTab::drop_commit` (3), `repo::op::RepoTab::drop_menu` (4), `repo::rebase::head_state` (12), `ui::dialogs::commit_drop_menu` (3), `ui::graph_view::dnd::branch_dnd` (3), `ui::graph_view::dnd::preview` (3)

### Smart rewrites

#### Split a commit (guided) `rewrite.split`
- **Entry:** src/ui/graph_view/menus.rs:282 Rewrite this commit → "Split this commit…"; WIP panel banner "Continue" / "Abort" (src/ui/inspector/wip.rs:261)
- **Platform:** `platform.git-runner`, `platform.offer`, `platform.op-runner`, `platform.undo-engine`, `platform.watcher`
- **Own code:** `repo::rebase::split`, `repo::split::RepoTab::begin_split`, `repo::split::RepoTab::end_split`, `repo::split::RepoTab::finish_split`, `repo::split::RepoTab::fold_split`, `repo::split::RepoTab::split_aborted`, `repo::split::RepoTab::split_replayed`, `repo::split::RepoTab::start_split`
- **Shared:** `repo::RepoTab::apply` (21), `repo::rebase::Editor::load` (6), `repo::rebase::Editor::op` (6), `repo::rebase::confirm_pushed` (7), `repo::rebase::confirm_revs` (7), `repo::rebase::head_state` (12), `repo::rebase::items` (7), `repo::rebase::rewrites_pushed` (7), `repo::rebase::sh_quote` (6), `repo::rebase::stacked_refs` (7), `ui::graph_view::menus::commit_menu` (24), `ui::inspector::wip::RepoTab::split_banner` (2)

#### Absorb staged hunks into the commits they belong to `rewrite.absorb`
- **Entry:** WIP panel "Absorb" button (src/ui/inspector/wip.rs:332); ⌘P "Absorb staged changes…" (palette.rs:114); preview dialog "Absorb N hunks into M commits" / "Cancel" (dialogs.rs:396)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.task`, `platform.undo-engine`
- **Own code:** `repo::absorb::Plan::load`, `repo::absorb::Plan::op`, `repo::absorb::Plan::targets`, `repo::absorb::RepoTab::poll_absorb`, `repo::absorb::RepoTab::start_absorb`, `repo::absorb::attribute`, `repo::absorb::parse_blame`, `repo::absorb::short`, `ui::dialogs::absorb_dialog`
- **Shared:** `repo::rebase::head_state` (12), `repo::rebase::no_hooks` (3), `repo::rebase::pending_autosquash` (2), `ui::inspector::wip::RepoTab::wip_panel` (31)

#### Commit Composer (AI plans commits from uncommitted changes) `rewrite.compose`
- **Entry:** WIP panel "Compose commits" (wip.rs:457); ⌘P "Compose commits with AI…" (palette.rs:122); dialog: edit/move units, merge up, "Create commits" (src/ui/compose.rs)
- **Platform:** `platform.ai-client`, `platform.git-runner`, `platform.op-runner`, `platform.task`, `platform.undo-engine`
- **Shared:** `ai::tasks::build` (5), `ai::tasks::compose_schema` (3), `ai::tasks::start` (5), `ai::tasks::uncommitted_changes` (2), `repo::RepoTab::apply` (21), `repo::ai_job::RepoTab::ai_busy` (3), `repo::ai_job::RepoTab::poll_ai` (4), `repo::ai_job::RepoTab::start_ai` (3), `repo::compose::Plan::fill` (2), `repo::compose::Plan::load` (2), `repo::compose::Plan::merge_up` (2), `repo::compose::Plan::move_unit` (2), `repo::compose::Plan::op` (2), `repo::compose::RepoTab::composing` (2), `repo::compose::RepoTab::poll_compose` (2), `repo::compose::RepoTab::start_compose` (2), `repo::compose::changes` (2), `repo::compose::render` (2), `repo::compose::repair` (2), `repo::rebase::head_state` (12), `ui::compose::compose_dialog` (2), `ui::inspector::wip::RepoTab::wip_panel` (31)

#### Recompose existing unpushed commits with the Composer `rewrite.recompose`
- **Entry:** graph commit menu "Recompose from here…" (menus.rs:287); multi menu "Recompose {n} commits…" (menus.rs:40); optional "(with uncommitted changes)"
- **Platform:** `platform.ai-client`, `platform.git-runner`, `platform.op-runner`, `platform.task`, `platform.undo-engine`
- **Own code:** `repo::compose::range`
- **Shared:** `ai::tasks::build` (5), `ai::tasks::compose_schema` (3), `ai::tasks::start` (5), `repo::ai_job::RepoTab::ai_free` (3), `repo::ai_job::RepoTab::poll_ai` (4), `repo::compose::Plan::fill` (2), `repo::compose::Plan::load` (2), `repo::compose::Plan::merge_up` (2), `repo::compose::Plan::move_unit` (2), `repo::compose::Plan::op` (2), `repo::compose::RepoTab::composing` (2), `repo::compose::RepoTab::poll_compose` (2), `repo::compose::RepoTab::start_compose` (2), `repo::compose::changes` (2), `repo::compose::render` (2), `repo::compose::repair` (2), `ui::compose::compose_dialog` (2), `ui::graph_view::menus::commit_menu` (24), `ui::graph_view::menus::multi_menu` (7), `ui::graph_view::menus::recompose_items` (3)

#### Move unpushed commits to a new branch `rewrite.spinoff`
- **Entry:** toolbar history tools (src/ui/toolbar.rs:178); branch context menu on HEAD (src/ui/sidebar/ref_menus.rs:248); ⌘P (src/ui/palette.rs:151)
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::spinoff::move_unpushed` (2), `ui::palette::items` (25), `ui::sidebar::ref_menus::local_menu` (22), `ui::toolbar::history_tools` (7)

### Stacked branches

#### Stacked branch detection and sidebar tree `rewrite.stack-detect`
- **Entry:** sidebar Branches list nests stacked branches under their parent, stale badge (src/ui/sidebar/mod.rs:88 stacked, :113 local_row); Settings → "Keep stacked branches together" / Protected branches (set_follow mirror in src/ui/app.rs:510); reload (repo/data.rs:523 stack::load) and after reload keep_stale (repo/mod.rs:425)
- **Platform:** `platform.git-model`, `platform.git-runner`, `platform.repo-load`
- **Own code:** `repo::stack::keep_stale`, `repo::stack::load`, `repo::stack::parse`, `repo::stack::reaches`, `repo::stack::set_follow`
- **Shared:** `repo::stack::children` (5), `repo::stack::follows` (2), `repo::stack::members` (5), `repo::stack::parent` (7), `repo::stack::protected` (2), `repo::stack::stale` (6), `repo::stack::trunk` (3), `repo::stack::trunk_name` (3), `ui::sidebar::local_row` (7), `ui::sidebar::stacked` (2), `ui::sidebar::stacked::add` (2)

#### Stacks move together on commit / rebase / pull / reword `rewrite.stack-follow`
- **Entry:** automatic: every commit, rebase, pull, merge or REWRITE op in a repo with stacks gets --update-refs and a trailing restack (stack::follow in RepoTab::run); interactive-rebase and reword ops keep stacked refs on rewritten commits (stacked_refs, update-ref lines)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::stack::follow`
- **Shared:** `repo::rebase::rewrite` (3), `repo::rebase::stacked_refs` (7), `repo::stack::children` (5), `repo::stack::follows` (2), `repo::stack::members` (5), `repo::stack::parent` (7), `repo::stack::protected` (2), `repo::stack::restack` (2), `repo::stack::restack_cmd` (2), `repo::stack::stale` (6), `repo::stack::subcommand` (2), `repo::stack::trunk` (3), `repo::stack::trunk_name` (3)

#### Restack a stack `rewrite.stack-restack`
- **Entry:** branch context menu "Restack" (src/ui/sidebar/ref_menus.rs:206), shown when any member is stale
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::stack::children` (5), `repo::stack::members` (5), `repo::stack::parent` (7), `repo::stack::restack` (2), `repo::stack::restack_cmd` (2), `repo::stack::restack_op` (2), `repo::stack::stale` (6), `repo::stack::subcommand` (2), `repo::stack::trunk` (3), `repo::stack::trunk_name` (3), `ui::sidebar::ref_menus::local_menu` (22)

#### Push a whole stack `rewrite.stack-push`
- **Entry:** branch context menu "Push stack (N branches)" (src/ui/sidebar/ref_menus.rs:214)
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::stack::children` (5), `repo::stack::members` (5), `repo::stack::parent` (7), `repo::stack::push_op` (3), `repo::stack::stale` (6), `ui::sidebar::ref_menus::local_menu` (22)

<a id="merge"></a>
## Merging, cherry-picks and conflicts

### Merge

#### Merge a branch into the current one (default) `merge.merge-branch`
- **Entry:** src/ui/graph_view/menus.rs:107 "Merge <target> into <head>" (commit/branch context menu, integrate_items); src/ui/palette.rs:201 "Merge <ref> into <head>" (palette); src/ui/dialogs.rs:498 branch drag-and-drop drop menu "Merge <src> into <dst>"
- **Platform:** `platform.git-runner`, `platform.offer`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::merge_opts::Merge::name` (4), `repo::merge_opts::merge_op` (4), `ui::dialogs::drop_menu` (5), `ui::graph_view::menus::commit_menu` (24), `ui::graph_view::menus::integrate_items` (6), `ui::palette::items` (25)

#### Merge with options (no-ff / ff-only / squash) `merge.merge-options`
- **Entry:** src/ui/graph_view/menus.rs:110 "Merge <target> into <head> with options" > Merge (no-ff|ff-only|squash); src/ui/palette.rs:201 palette items with (squash)/(no-ff)/(ff-only); src/ui/dialogs.rs:496 drop menu merge variants (squash only onto the checked-out branch)
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::merge_opts::Merge::name` (4), `repo::merge_opts::merge_op` (4), `ui::dialogs::drop_menu` (5), `ui::graph_view::menus::integrate_items` (6)

#### Squash merge pre-fills the commit box `merge.squash-message`
- **Entry:** background: after a successful `merge --squash` op (RepoTab::run puts git's SQUASH_MSG in the commit message box)
- **Platform:** `platform.op-runner`
- **Own code:** `repo::merge_opts::squash_message`
- **Shared:** `repo::op::RepoTab::run` (5)

### Cherry-pick and revert

#### Cherry-pick a commit `merge.cherry-pick`
- **Entry:** src/ui/graph_view/menus.rs:203 "Cherry-pick commit" (commit menu; -m for merge commits); setting cherry_pick_x (-x) via src/ui/app.rs:509
- **Platform:** `platform.offer`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::merge_opts::pick_x`, `repo::merge_opts::set_pick_x`
- **Shared:** `repo::merge_opts::clean` (4), `repo::merge_opts::pick_new` (4), `repo::merge_opts::pick_op` (5), `repo::op::exec` (7), `ui::graph_view::menus::commit_menu` (24)

#### Cherry-pick without committing `merge.cherry-pick-no-commit`
- **Entry:** src/ui/graph_view/menus.rs:207 "Cherry-pick without committing"
- **Platform:** `platform.offer`, `platform.op-runner`
- **Shared:** `repo::merge_opts::clean` (4), `repo::merge_opts::pick_new` (4), `repo::merge_opts::pick_op` (5), `repo::op::exec` (7), `ui::graph_view::menus::commit_menu` (24)

#### Cherry-pick several selected commits `merge.cherry-pick-multi`
- **Entry:** src/ui/graph_view/menus.rs:20 "Cherry-pick N commits" (multi-select menu); src/ui/inspector/multi.rs multi panel Cherry-pick button
- **Platform:** `platform.offer`, `platform.op-runner`
- **Shared:** `repo::merge_opts::clean` (4), `repo::merge_opts::pick_new` (4), `repo::merge_opts::pick_op` (5), `repo::op::RepoTab::multi_op` (4), `repo::op::exec` (7), `ui::graph_view::menus::multi_menu` (7), `ui::inspector::multi::RepoTab::multi_panel` (4)

#### Cherry-pick by dragging a commit onto a branch `merge.cherry-pick-drag`
- **Entry:** src/repo/op.rs:1494 drop menu "Cherry-pick onto <branch>" (drag a commit row onto a branch pill / sidebar row)
- **Platform:** `platform.offer`, `platform.op-runner`
- **Shared:** `repo::merge_opts::clean` (4), `repo::merge_opts::pick_new` (4), `repo::merge_opts::pick_op` (5), `repo::op::RepoTab::drop_menu` (4), `repo::op::exec` (7)

#### Revert a commit `merge.revert-commit`
- **Entry:** src/ui/graph_view/menus.rs:219 "Revert commit" (merge commits ask first, parent 1)
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `ui::graph_view::menus::commit_menu` (24)

#### Revert several selected commits `merge.revert-multi`
- **Entry:** src/ui/graph_view/menus.rs:22 "Revert N commits"; src/ui/inspector/multi.rs:22 Revert button
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::op::RepoTab::multi_op` (4), `ui::graph_view::menus::multi_menu` (7), `ui::inspector::multi::RepoTab::multi_panel` (4)

### Conflict state and resolution

#### In-progress banner: Continue / Skip / Abort (merge, rebase, cherry-pick, revert, am) `merge.state-banner`
- **Entry:** src/ui/inspector/wip.rs:299 WIP panel banner "<Merge> in progress": Continue, Skip this commit, Abort
- **Platform:** `platform.op-runner`, `platform.repo-load`, `platform.undo-engine`
- **Own code:** `repo::op::ends_rebase`
- **Shared:** `git::read::state` (3), `repo::op::continue_op` (2), `repo::op::in_progress` (4), `repo::op::sequencer_op` (2), `ui::inspector::wip::RepoTab::wip_panel` (31), `ui::inspector::wip::open_tools` (5)

#### Conflicted files list with kind labels `merge.conflict-list`
- **Entry:** src/ui/inspector/wip.rs:475 WIP panel "Conflicts" tree (click opens merge editor); background: RepoData::load_conflicts on every reload
- **Platform:** `platform.git-model`, `platform.repo-load`
- **Shared:** `git::read::conflict_kind` (5), `repo::data::RepoData::load_conflicts` (2), `repo::merge_editor::gitlinks` (5), `repo::merge_editor::labels` (4), `repo::merge_editor::resolvable` (2), `ui::inspector::wip::RepoTab::wip_panel` (31)

#### Use current / incoming version of a file `merge.take-side`
- **Entry:** src/ui/inspector/wip.rs:527 conflict file context menu "Use current (<label>)" / "Use incoming (<label>)"
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `git::read::conflict_kind` (5), `repo::merge_editor::gitlinks` (5), `repo::merge_editor::labels` (4), `repo::merge_editor::overwrites` (2), `repo::merge_editor::take_side` (3), `ui::inspector::wip::RepoTab::wip_panel` (31)

#### Mark file / folder resolved `merge.mark-resolved`
- **Entry:** src/ui/inspector/wip.rs conflict file menu "Mark resolved"; folder menu "Mark folder resolved"; src/ui/merge_editor.rs:31 after Save & mark resolved
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::merge_editor::attrs_many`
- **Shared:** `repo::merge_editor::attrs` (2), `repo::merge_editor::is_marker` (3), `repo::merge_editor::like` (2), `repo::merge_editor::marker_size` (3), `repo::merge_editor::resolvable` (2), `repo::merge_editor::stage` (3), `repo::merge_editor::stage_many` (2), `ui::inspector::wip::RepoTab::wip_panel` (31), `ui::inspector::wip::mark_resolved_op` (2), `ui::inspector::wip::shown` (2)

#### Restore conflict markers `merge.restore-markers`
- **Entry:** src/ui/inspector/wip.rs:547 conflict file menu "Restore conflict markers"
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::tools::restore_markers_op` (3), `ui::inspector::wip::RepoTab::wip_panel` (31)

#### Predicted-conflicts chip in toolbar `merge.predict-conflicts`
- **Entry:** src/ui/toolbar.rs:103 chip "N conflicts with <branch>"; background: after each reload (RepoTab::poll -> check_conflicts)
- **Platform:** `platform.git-runner`, `platform.repo-load`, `platform.task`
- **Own code:** `git::read::predict_conflicts`, `repo::conflicts::RepoTab::check_conflicts`
- **Shared:** `repo::RepoTab::poll` (3), `ui::toolbar::toolbar` (8)

### Merge editor

#### Open the three-pane merge editor `merge.editor-open`
- **Entry:** click a conflicted file in the WIP panel (Action::MergeEditor), src/ui/inspector/wip.rs:552; src/ui/repo_view.rs:61 center pane
- **Platform:** `platform.design-system`, `platform.git-runner`, `platform.task`
- **Own code:** `repo::merge_editor::Editor::fill`, `repo::merge_editor::Editor::open`, `repo::merge_editor::conflicts`, `repo::merge_editor::load`, `ui::merge_editor::tint`
- **Shared:** `repo::RepoTab::apply` (21), `repo::merge_editor::Editor::reparse` (4), `repo::merge_editor::attrs` (2), `repo::merge_editor::binary_stages` (2), `repo::merge_editor::block` (2), `repo::merge_editor::decode` (2), `repo::merge_editor::is_marker` (3), `repo::merge_editor::labels` (4), `repo::merge_editor::like` (2), `repo::merge_editor::marker_size` (3), `repo::merge_editor::parse` (2), `repo::merge_editor::parts` (2), `ui::merge_editor::show` (7)

#### Take current / incoming / both / base per conflict `merge.editor-resolve`
- **Entry:** src/ui/merge_editor.rs:212 chunk side click; footer buttons "Take current", "Take incoming", "Both (current first|incoming first)", "Take base"
- **Platform:** `platform.design-system`
- **Own code:** `repo::merge_editor::Conflict::take`, `repo::merge_editor::Editor::resolve`
- **Shared:** `repo::merge_editor::Editor::output_line` (2), `repo::merge_editor::Editor::reparse` (4), `repo::merge_editor::Editor::rewrite` (2), `repo::merge_editor::block` (2), `repo::merge_editor::parts` (2), `repo::merge_editor::rebuild` (2), `ui::merge_editor::show` (7)

#### Auto-resolve trivial conflicts `merge.editor-auto-trivial`
- **Entry:** src/ui/merge_editor.rs:64 "Auto-resolve trivial (N)"
- **Platform:** `platform.design-system`
- **Own code:** `repo::merge_editor::Conflict::trivial`, `repo::merge_editor::Conflict::trivial::words`, `repo::merge_editor::Editor::resolve_trivial`
- **Shared:** `repo::merge_editor::Editor::reparse` (4), `repo::merge_editor::Editor::rewrite` (2), `ui::merge_editor::show` (7)

#### Edit the merged output by hand `merge.editor-edit-output`
- **Entry:** src/ui/merge_editor.rs output text pane
- **Platform:** `platform.design-system`
- **Own code:** `repo::merge_editor::Editor::edited`
- **Shared:** `repo::merge_editor::Editor::output_line` (2), `repo::merge_editor::Editor::reparse` (4), `repo::merge_editor::parse` (2), `repo::merge_editor::rebuild` (2), `ui::merge_editor::show` (7)

#### Save & mark resolved `merge.editor-save`
- **Entry:** src/ui/merge_editor.rs:289 "Save & mark resolved"; "Close without saving"; X
- **Platform:** `platform.op-runner`, `platform.task`, `platform.undo-engine`
- **Own code:** `repo::merge_editor::new_stray`, `repo::merge_editor::save`, `repo::merge_editor::stray`
- **Shared:** `repo::merge_editor::decode` (2), `repo::merge_editor::is_marker` (3), `repo::merge_editor::marker_size` (3), `repo::merge_editor::stage` (3), `ui::merge_editor::show` (7)

#### Pick a whole version (delete/binary/gitlink/mode conflicts) `merge.editor-whole-file`
- **Entry:** src/ui/merge_editor.rs:84 buttons "Take current · <label>" / "Take incoming · <label>" (or "Delete file" / "Keep file" when one side deleted it) shown when a file can't be merged by hunks
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::merge_editor::whole_ops`
- **Shared:** `git::read::conflict_kind` (5), `repo::merge_editor::binary_stages` (2), `repo::merge_editor::gitlinks` (5), `repo::merge_editor::labels` (4), `repo::merge_editor::overwrites` (2), `repo::merge_editor::take_side` (3), `ui::merge_editor::show` (7)

#### AI conflict resolve (editor button and file menu) `merge.ai-resolve`
- **Entry:** src/ui/merge_editor.rs:61 "Resolve" sparkle button; src/ui/inspector/wip.rs:520 conflict file menu "Resolve…"; src/ui/dialogs.rs:215 AI result dialog "Apply & mark resolved"
- **Platform:** `platform.ai-client`, `platform.op-runner`, `platform.task`
- **Own code:** `repo::merge_editor::apply_ai`
- **Shared:** `ai::tasks::Job::poll` (3), `ai::tasks::Job::running` (3), `ai::tasks::Purpose::title` (3), `ai::tasks::build` (5), `ai::tasks::start` (5), `repo::ai_job::RepoTab::ai_busy` (3), `repo::ai_job::RepoTab::ai_free` (3), `repo::ai_job::RepoTab::cancel_ai` (2), `repo::ai_job::RepoTab::poll_ai` (4), `repo::ai_job::RepoTab::start_ai` (3), `repo::merge_editor::stage` (3), `repo::tools::RepoTab::merge_tool_open` (3), `ui::dialogs::ai_dialog` (4), `ui::inspector::wip::RepoTab::wip_panel` (31), `ui::merge_editor::show` (7)

#### Open in external merge tool (git mergetool --gui) `merge.external-tool`
- **Entry:** src/ui/inspector/wip.rs:537 conflict menu "Open in external merge tool"; Cancel in the banner (open_tools)
- **Platform:** `platform.op-runner`, `platform.task`, `platform.undo-engine`
- **Shared:** `repo::op::RepoTab::run` (5), `repo::tools::RepoTab::close_tool` (2), `repo::tools::RepoTab::merge_tool_open` (3), `repo::tools::RepoTab::open_tool` (3), `repo::tools::RepoTab::poll_tools` (3), `repo::tools::RepoTab::tool_blocks` (2), `repo::tools::kind` (3), `repo::tools::mergetool_op` (3), `repo::tools::tool` (3), `repo::tools::touches` (2), `ui::inspector::wip::RepoTab::wip_panel` (31), `ui::inspector::wip::open_tools` (5)

### Bisect

#### Start a bisect `merge.bisect-start`
- **Entry:** src/ui/bisect.rs:72 commit menu "Bisect" > "Start bisect: this commit is bad" / "is good"; src/ui/bisect.rs:96 multi-select "Start bisect between these"
- **Platform:** `platform.op-runner`
- **Own code:** `ui::bisect::multi_item`
- **Shared:** `ui::bisect::commit_items` (2), `ui::graph_view::menus::commit_menu` (24), `ui::graph_view::menus::multi_menu` (7)

#### Mark good / bad / skip `merge.bisect-mark`
- **Entry:** src/ui/bisect.rs:51 banner pills Skip / Bad / Good; src/ui/bisect.rs:84 commit menu "Mark <sha> as good|bad|skip"
- **Platform:** `platform.op-runner`
- **Shared:** `git::bisect::Bisect::term` (2), `ui::bisect::banner` (2), `ui::bisect::commit_items` (2), `ui::bisect::mark_color` (3), `ui::graph_view::menus::commit_menu` (24)

#### Bisect progress banner (steps left, first bad commit, Show, Reset) `merge.bisect-banner`
- **Entry:** src/ui/graph_view/mod.rs:154 banner above the graph; Reset pill; Show jumps to the found commit
- **Platform:** `platform.design-system`, `platform.op-runner`
- **Own code:** `git::bisect::Bisect::found`
- **Shared:** `git::bisect::Bisect::term` (2), `ui::bisect::banner` (2), `ui::bisect::mark_color` (3), `ui::graph_view::show` (17)

#### Bisect marks on graph rows `merge.bisect-graph-marks`
- **Entry:** src/ui/graph_view/mod.rs:573 coloured good/bad/skip markers
- **Platform:** `platform.design-system`
- **Own code:** `git::bisect::Bisect::mark`
- **Shared:** `ui::bisect::mark_color` (3), `ui::graph_view::show` (17)

#### Bisect progress tracking and follow-HEAD (background) `merge.bisect-state`
- **Entry:** background: every reload (RepoData::load_bisect); RepoTab::poll jumps the graph to the new HEAD after each step
- **Platform:** `platform.git-runner`, `platform.repo-load`
- **Own code:** `git::bisect::bisect`, `git::bisect::parse_refs`, `git::bisect::steps`
- **Shared:** `git::read::state` (3), `repo::RepoTab::poll` (3), `repo::data::RepoData::load_bisect` (2)

<a id="remote"></a>
## Remotes, sync and hosting

### Fetch, pull and push

#### Fetch all remotes `remote.fetch`
- **Entry:** src/ui/toolbar.rs:238 toolbar "Fetch" pill; src/ui/app.rs:221 shortcut Cmd+L; src/ui/palette.rs:79 Cmd+P "Fetch all remotes"; src/ui/toolbar.rs:112 click the failed-fetch chip; src/repo/offer.rs:49 "Fetch" choice after a rejected push
- **Platform:** `platform.git-runner`, `platform.offer`, `platform.op-runner`
- **Own code:** `repo::op::fetch_op`
- **Shared:** `git::network::tracking` (2), `repo::fetch::Fetching::user` (3), `repo::offer::push_rejected` (3), `ui::palette::item` (2), `ui::palette::items` (25), `ui::toolbar::label` (3), `ui::toolbar::toolbar_remote` (5)

#### Fetch or prune a single remote `remote.fetch-one`
- **Entry:** src/ui/sidebar/remotes.rs:12 remote context menu "Fetch <remote>"; src/ui/sidebar/remotes.rs:17 "Prune deleted branches"; src/ui/sidebar/mod.rs:281 right-click a remote header
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Own code:** `repo::op::fetch_remote_op`, `repo::op::prune_remote_op`, `repo::op::remote_specs`
- **Shared:** `ui::sidebar::remote_row` (6), `ui::sidebar::remotes::remote_menu` (7)

#### Pull (merge, fast-forward only, rebase) with autostash `remote.pull`
- **Entry:** src/ui/toolbar.rs:241 "Pull" pill (default mode from Settings); src/ui/toolbar.rs:245 caret menu "Pull with" Merge/Fast-forward only/Rebase; src/ui/palette.rs:80 Cmd+P "Pull" / "Pull (fast-forward only)" / "Pull (rebase)"; src/ui/settings.rs:220 Settings "Default pull"; src/repo/offer.rs:12 "Pull (rebase)"/"Pull (merge)" choices
- **Platform:** `platform.git-runner`, `platform.offer`, `platform.op-runner`
- **Own code:** `repo::op::pull_op`
- **Shared:** `git::network::with_autostash` (2), `repo::offer::pull_choices` (2), `ui::palette::items` (25), `ui::settings::App::settings_dialog` (10), `ui::toolbar::pull_hint` (2), `ui::toolbar::toolbar_remote` (5)

#### Fast-forward a branch without checking it out `remote.pull-ff-branch`
- **Entry:** src/ui/sidebar/ref_menus.rs:188 local branch menu "Pull <b> (fast-forward, without checkout)"; src/ui/sidebar/ref_menus.rs:185 "Fast-forward <b> to <upstream>" (local fetch)
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::data::RepoData::split_remote` (13), `repo::remotes::pull_ff` (2), `ui::sidebar::ref_menus::local_menu` (22), `ui::sidebar::ref_menus::ref_menu` (4)

#### Push the current branch (sets upstream on first push) `remote.push`
- **Entry:** src/ui/toolbar.rs:256 "Push" pill (opens Add remote when none); src/ui/palette.rs:86 Cmd+P "Push"; src/ui/inspector/wip.rs:404 "Push after commit" checkbox (the commit button then reads "Commit and push changes to N files", wip.rs:415); src/repo/op.rs:1377 push after a queued commit
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::data::RepoData::cfg` (3), `repo::data::RepoData::default_remote` (8), `repo::data::RepoData::push_dest` (3), `repo::data::RepoData::push_target` (4), `repo::data::RepoData::split_remote` (13), `repo::data::split_remote` (2), `repo::op::RepoData::push_op` (2), `repo::op::RepoTab::push_op` (2), `repo::remotes::pr_push_refused` (3), `ui::palette::items` (25), `ui::toolbar::label` (3), `ui::toolbar::toolbar_remote` (5)

#### Force push with lease `remote.force-push`
- **Entry:** src/ui/toolbar.rs:268 push caret menu "Force push (with lease)"; src/ui/palette.rs:89 Cmd+P "Force push (with lease)"; src/repo/offer.rs:52 "Force push…" choice after a rejected push
- **Platform:** `platform.git-runner`, `platform.offer`, `platform.op-runner`
- **Shared:** `repo::data::RepoData::cfg` (3), `repo::data::RepoData::default_remote` (8), `repo::data::RepoData::push_dest` (3), `repo::offer::push_rejected` (3), `repo::op::RepoData::push_op` (2), `repo::op::RepoTab::push_op` (2), `ui::palette::items` (25), `ui::toolbar::toolbar_remote` (5)

#### Push a specific branch or tag from the sidebar `remote.push-branch`
- **Entry:** src/ui/sidebar/ref_menus.rs:195 local branch menu "Push <b> to <remote>"; src/ui/sidebar/ref_menus.rs:331 tag menu "Push tag to <remote>"; src/ui/sidebar/ref_menus.rs:334 tag menu "Push all tags to <remote>" (also Cmd+P); src/ui/graph_view pills open the same ref_menu
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::data::RepoData::default_remote` (8), `repo::data::RepoData::push_target` (4), `repo::tags::push_all` (2), `ui::sidebar::ref_menus::local_menu` (22), `ui::sidebar::ref_menus::tag_menu` (11)

#### Push a stack of branches `remote.push-stack`
- **Entry:** src/ui/sidebar/ref_menus.rs:214 local branch menu "Push stack (N branches)"
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::remotes::pr_push_refused` (3), `repo::stack::push_op` (3), `ui::sidebar::ref_menus::local_menu` (22)

#### Delete a branch from the remote `remote.delete-remote-branch`
- **Entry:** src/ui/sidebar/ref_menus.rs:60 delete_items "Delete <b> from <remote>" (remote branch menu and synced graph pill, last)
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Shared:** `repo::data::RepoData::split_remote` (13), `repo::remotes::delete_branch` (3), `ui::sidebar::ref_menus::delete_items` (8), `ui::sidebar::ref_menus::synced_menu` (5)

### Background sync

#### Auto-fetch in the background `remote.auto-fetch`
- **Entry:** src/ui/app.rs:531 per-frame tick calls RepoTab::auto_fetch (timer and window refocus); src/ui/settings.rs:210 Settings "Auto-fetch" minutes (0 = off); src/ui/toolbar.rs:109 failed-fetch chip "Sign in required"/"Fetch failed" (click to fetch now); src/ui/toolbar.rs:117 chip "Choose SSH key"
- **Platform:** `platform.git-runner`, `platform.repo-load`, `platform.task`
- **Own code:** `git::network::fetch_failure`, `git::network::fetch_quiet`, `git::network::fetch_specs`, `repo::fetch::RepoTab::auto_fetch`
- **Shared:** `git::network::batch_ssh` (2), `git::network::quiet` (2), `git::network::quiet_ssh` (2), `git::network::tracking` (2), `git::network::unquoted` (2), `repo::fetch::Fetching::background` (2), `repo::fetch::Fetching::drop` (2), `repo::fetch::Fetching::user` (3), `ui::settings::App::settings_dialog` (10), `ui::toolbar::toolbar` (8)

#### Fetch coordination (a user fetch pre-empts a background one) `remote.one-fetch-at-a-time`
- **Entry:** background: every Op run takes a user turn (src/repo/op.rs:1158)
- **Platform:** `platform.op-runner`
- **Shared:** `repo::fetch::Fetching::background` (2), `repo::fetch::Fetching::drop` (2), `repo::fetch::Fetching::user` (3)

#### Autostash around pull/rebase/restack `remote.autostash`
- **Entry:** src/repo/op.rs:51 Op.autostash (pull, restack); src/repo/offer.rs:25 autostash_checkout: "Stash, switch and reapply" choice when a checkout is blocked
- **Platform:** `platform.git-runner`, `platform.offer`, `platform.op-runner`
- **Shared:** `git::network::with_autostash` (2), `repo::offer::autostash_checkout` (3)

### Remotes

#### Add or edit a remote (name, fetch URL, push URL) `remote.add-edit-remote`
- **Entry:** src/ui/sidebar/mod.rs:293 sidebar "Add remote…"; src/ui/sidebar/remotes.rs:37 remote menu "Edit remote…"; src/ui/palette.rs:172 Cmd+P "Add remote…"; src/ui/toolbar.rs:263 Push with no remote; src/ui/graph_view/empty.rs:50 empty-repo hint; src/ui/repo_view.rs:134 dialog host
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::remotes::Form::add`, `repo::remotes::Form::edit`, `repo::remotes::Form::op`, `repo::remotes::Form::op::set_push`, `repo::remotes::Form::problem`, `repo::remotes::seq`, `ui::sidebar::remotes::form_dialog`
- **Shared:** `git::read::remotes` (3), `git::url::redact` (6), `ui::palette::items` (25), `ui::sidebar::remotes::remote_menu` (7)

#### Remove a remote (undoable) `remote.remove-remote`
- **Entry:** src/ui/sidebar/remotes.rs:51 remote menu "Remove remote…"
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::remotes::remove`, `repo::remotes::reset`
- **Shared:** `ui::sidebar::remotes::remote_menu` (7)

#### Remotes in the sidebar (list, redacted URL tooltip) `remote.remote-list`
- **Entry:** src/ui/sidebar/mod.rs:260 sidebar REMOTES section
- **Platform:** `platform.repo-load`
- **Shared:** `git::read::remotes` (3), `git::url::redact` (6), `git::url::secrets` (3), `git::url::shell_words` (3), `repo::data::RepoData::split_remote` (13), `ui::sidebar::remote_row` (6)

### Pull requests and web links

#### Check out a pull request by number `remote.pr-checkout`
- **Entry:** src/ui/sidebar/remotes.rs:21 remote menu "Check out pull request #…"; src/ui/palette.rs:176 Cmd+P "Check out PR #…"
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Own code:** `repo::remotes::pr_checkout`, `repo::remotes::pr_guard`
- **Shared:** `repo::remotes::pr_head` (2), `repo::remotes::pr_refspec` (2), `ui::palette::items` (25), `ui::sidebar::remotes::remote_menu` (7)

#### Show pull requests as <remote>/pr/<n> `remote.pr-refs`
- **Entry:** src/ui/sidebar/remotes.rs:27 remote menu checkbox "Show pull requests as …/pr/<n>"
- **Platform:** `platform.git-runner`, `platform.op-runner`
- **Own code:** `repo::remotes::is_pr_head`, `repo::remotes::numbered`, `repo::remotes::pr_refs_clash`, `repo::remotes::pr_refs_on`, `repo::remotes::pr_refs_op`
- **Shared:** `repo::data::RepoData::cfg_all` (2), `repo::remotes::pr_head` (2), `repo::remotes::pr_push_refused` (3), `repo::remotes::pr_refspec` (2), `ui::sidebar::remotes::remote_menu` (7)

#### Create a pull request (opens the compare page) `remote.create-pr`
- **Entry:** src/ui/sidebar/ref_menus.rs:167 branch menu "Create pull request…"; src/ui/palette.rs:187 Cmd+P "Create pull request for <branch>"; src/ui/dialogs.rs:198 AI dialog "Open pull request page" (with title/body)
- **Platform:** `platform.ai-client`
- **Shared:** `git::url::host` (3), `git::url::url_encode` (3), `git::url::url_path` (3), `git::url::web_url` (2), `repo::data::RepoData::default_branch` (4), `repo::data::RepoData::web` (4), `repo::data::RepoData::web_of` (4), `repo::stack::parent` (7), `ui::dialogs::ai_dialog` (4), `ui::palette::items` (25), `ui::sidebar::ref_menus::pr_base` (2), `ui::sidebar::ref_menus::pull_request_url` (2), `ui::sidebar::ref_menus::remote_branch` (2), `ui::sidebar::ref_menus::web_items` (3)

#### Open on remote / Copy link (repo, branch, commit, file, blame, history, compare, release) `remote.open-on-remote`
- **Entry:** src/ui/toolbar.rs:30 globe pill; src/ui/palette.rs:183 Cmd+P "Open repository on remote"; src/ui/sidebar/ref_menus.rs:172 "Open branch on remote"; src/ui/sidebar/remotes.rs:43 "Open on <host>"; src/ui/links.rs:11 "Open … on <host>" / "Copy link to …" in commit, file, diff-line, compare, tag and multi-select menus; src/ui/graph_view/menus.rs:30 (multi-select compare), :237 (commit menu); src/ui/inspector/mod.rs:84 permalink_items; src/ui/diff_view.rs:365 line permalinks; src/ui/sidebar/ref_menus.rs:344 tag "release page"
- **Platform:** `platform.repo-load`
- **Shared:** `git::url::Page::url` (4), `git::url::host` (3), `git::url::url_encode` (3), `git::url::url_path` (3), `git::url::web_url` (2), `repo::data::RepoData::on_remote` (2), `repo::data::RepoData::on_remote_in_graph` (2), `repo::data::RepoData::reachable` (3), `repo::data::RepoData::web` (4), `repo::data::RepoData::web_of` (4), `repo::data::RepoData::webs` (2), `ui::inspector::permalink_items` (3), `ui::links::items` (3), `ui::palette::items` (25), `ui::sidebar::ref_menus::tag_menu` (11), `ui::sidebar::ref_menus::web_items` (3), `ui::sidebar::remotes::remote_menu` (7), `ui::toolbar::toolbar` (8)

#### Only open http(s) pages on a remote's host (Action::Url guard) `remote.url-safety`
- **Entry:** src/repo/mod.rs:731 Action::Url handling
- **Platform:** `platform.op-runner`
- **Own code:** `git::url::is_web_link`
- **Shared:** `git::url::host` (3)

#### #123 / GH-123 in commit messages link to the pull request `remote.autolinks`
- **Entry:** src/ui/inspector/commit.rs:17 commit message in the details panel
- **Own code:** `git::url::autolinks`
- **Shared:** `repo::data::RepoData::web` (4), `ui::inspector::commit::linked` (2)

#### Credentials hidden in URLs, command log and settings `remote.secret-redaction`
- **Entry:** src/git/log.rs:45 command log; src/git/runner.rs error messages; src/ui/sidebar/mod.rs:281 remote tooltip; src/ui/palette.rs:177; src/ui/doctor.rs:106
- **Platform:** `platform.git-runner`
- **Shared:** `git::url::redact` (6), `git::url::redact_command` (3), `git::url::secrets` (3), `git::url::shell_words` (3), `repo::doctor::redact_proxy` (2)

### Clone

#### Clone a repository `remote.clone`
- **Entry:** src/ui/welcome.rs:42 welcome "Clone…" button; src/ui/palette.rs Cmd+P clone (src/ui/app.rs:426 Cmd::Clone); src/ui/app.rs:582 dialog host
- **Platform:** `platform.git-runner`, `platform.task`
- **Own code:** `ui::welcome::App::clone_dialog`, `ui::welcome::App::open_clone`, `ui::welcome::clone_cleaning_up`, `ui::welcome::clone_dir`, `ui::welcome::home`

### Credentials and connection

#### Credential prompt window for HTTPS and SSH `remote.askpass`
- **Entry:** git/ssh spawn Glasir as GIT_ASKPASS / SSH_ASKPASS (src/git/runner.rs:68); src/main.rs:24 askpass dispatch
- **Platform:** `platform.git-runner`
- **Own code:** `askpass::run`
- **Shared:** `main` (5)

#### Git version check and login-shell environment import `remote.git-env`
- **Entry:** src/main.rs:44 import_login_env at start; src/ui/app.rs:91 git version check task
- **Platform:** `platform.task`
- **Shared:** `git::env::check_git` (3), `git::env::import_login_env` (3), `git::env::parse_env` (3), `git::env::parse_version` (3)

#### Connection doctor (Check connection…) `remote.doctor`
- **Entry:** src/ui/sidebar/remotes.rs:31 remote menu "Check connection…"; src/ui/palette.rs:177 Cmd+P "Check connection to <remote>"; src/ui/doctor.rs:56 "Check again" / "Copy report"; src/ui/toolbar.rs:117 "Choose SSH key" chip
- **Platform:** `platform.git-runner`, `platform.task`
- **Own code:** `git::network::diagnose`, `git::network::ls_remote_quiet`, `repo::doctor::Doctor::open`, `repo::doctor::check`, `repo::doctor::helper`, `repo::doctor::helpers`, `repo::doctor::url_matches`, `repo::doctor::url_matches::parts`, `ui::doctor::doctor_dialog`
- **Shared:** `git::network::batch_ssh` (2), `git::network::quiet` (2), `git::network::quiet_ssh` (2), `git::network::unquoted` (2), `git::url::redact` (6), `repo::doctor::Doctor::poll` (2), `repo::doctor::redact_proxy` (2), `ui::palette::items` (25), `ui::sidebar::remotes::remote_menu` (7)

#### Choose and test the SSH key for a repository `remote.ssh-key`
- **Entry:** src/ui/sidebar/remotes.rs:34 remote menu "SSH key…"; src/ui/palette.rs:180 Cmd+P "SSH key for this repository…"; src/ui/doctor.rs (ssh_key_section: key list, Browse…, Test connection); src/repo/mod.rs:858 Action::SshKey / SshTest
- **Platform:** `platform.op-runner`, `platform.task`, `platform.undo-engine`
- **Own code:** `git::network::ls_remote_via`, `repo::doctor::Doctor::test`, `repo::sshkey::build`, `repo::sshkey::home`, `repo::sshkey::is_ssh`, `repo::sshkey::list`, `repo::sshkey::parse`, `repo::sshkey::quiet`, `repo::sshkey::set_op`, `repo::sshkey::slashed`, `repo::sshkey::test`, `ui::doctor::ssh_key_section`
- **Shared:** `git::url::redact_command` (3), `repo::doctor::Doctor::poll` (2)

#### Saved HTTPS token lookup via git's credential helper (for avatars) `remote.token-lookup`
- **Entry:** src/repo/avatars.rs Lookup::token
- **Platform:** `platform.git-runner`
- **Own code:** `git::network::credential_token`

### Avatars

#### GitHub avatars for commit authors `remote.avatars`
- **Entry:** src/ui/settings.rs:195 Settings "Show GitHub avatars"; src/ui/app.rs:508 applies the setting; src/repo/mod.rs:544 sync after load; src/ui/inspector/commit.rs:248, :274 author / co-author in details; src/ui/graph_view/mod.rs:416 graph rows (circles; fallback color from theme::avatar)
- **Platform:** `platform.design-system`, `platform.git-runner`, `platform.task`
- **Own code:** `repo::avatars::Avatars::get`, `repo::avatars::Avatars::sync`, `repo::avatars::Lookup::api_get`, `repo::avatars::Lookup::blocked`, `repo::avatars::Lookup::download`, `repo::avatars::Lookup::get`, `repo::avatars::Lookup::lookup`, `repo::avatars::Lookup::miss`, `repo::avatars::Lookup::new`, `repo::avatars::Lookup::reachable`, `repo::avatars::Lookup::read`, `repo::avatars::Lookup::record`, `repo::avatars::Lookup::resolve`, `repo::avatars::Lookup::save`, `repo::avatars::Lookup::skipped`, `repo::avatars::Lookup::token`, `repo::avatars::api_base`, `repo::avatars::avatar_of`, `repo::avatars::blocks_host`, `repo::avatars::enabled`, `repo::avatars::file_name`, `repo::avatars::fresh`, `repo::avatars::github_repo`, `repo::avatars::image_ok`, `repo::avatars::is_image`, `repo::avatars::load_index`, `repo::avatars::noreply_url`, `repo::avatars::now_fresh`, `repo::avatars::retry_bare`, `repo::avatars::set_enabled`, `repo::avatars::sized`, `repo::avatars::texture`
- **Shared:** `theme::avatar` (2), `ui::inspector::commit::avatar` (3)

#### DiceBear critter pictures for authors without a GitHub avatar `remote.critters`
- **Entry:** graph rows: the author circle when there is no GitHub avatar (src/ui/graph_view/mod.rs:416), also with Settings → "Show GitHub avatars" off; commit details author / co-authors (src/ui/inspector/commit.rs:66 avatar); file history rows (src/ui/history.rs:119); background: rendered off the UI thread from the author's name, so the same in every repo (src/repo/avatars.rs:134 critter); PNG cached on disk for five years (avatars.rs:152 cached_critter); initials until it is ready
- **Platform:** `platform.design-system`
- **Own code:** `repo::avatars::cached_critter`, `repo::avatars::critter`, `repo::avatars::critter_image`
- **Shared:** `ui::graph_view::show` (17), `ui::inspector::commit::avatar` (3), `ui::history::show` (3)

### LFS and submodules

#### Git LFS awareness (missing-install warning, pointer diffs, track pattern) `remote.lfs`
- **Entry:** src/repo/data.rs:497 detect on load; src/ui/toolbar.rs:205 LFS tools (Pull/Fetch LFS objects) + missing-install warning; src/repo/mod.rs:439 startup warning; src/ui/inspector/mod.rs:93 "Track *.ext with LFS"; src/ui/diff_view.rs:407 LFS pointer note instead of diff
- **Platform:** `platform.git-model`, `platform.repo-load`
- **Own code:** `git::lfs::lfs`, `git::lfs::lfs_installed`, `git::lfs::lfs_paths`, `git::lfs::parse_pointer`, `git::lfs::pointer_diff`
- **Shared:** `ui::diff_view::lfs_note` (3), `ui::inspector::lfs_track_item` (3), `ui::toolbar::history_tools` (7)

#### Submodules: list, update/init, sync, add, remove, open `remote.submodules`
- **Entry:** src/ui/sidebar/mod.rs:320 SUBMODULES section; src/ui/sidebar/submodules.rs row context menu & double-click; src/ui/sidebar/submodules.rs:20 "Update all" / "Sync URLs" / "Add submodule…"
- **Platform:** `platform.op-runner`, `platform.repo-load`
- **Own code:** `git::submodule::parse_status`, `git::submodule::submodules`, `ui::sidebar::submodules::list`, `ui::sidebar::submodules::remove`, `ui::sidebar::submodules::row`

#### Submodule pointer change in a diff `remote.submodule-diff`
- **Entry:** src/ui/diff_view.rs:740 note "Submodule moved from … to …" with log and Open submodule in new tab; src/repo/merge_editor.rs gitlinks (conflicted submodule)
- **Platform:** `platform.git-runner`
- **Own code:** `git::submodule::parse_submodule_diff`
- **Shared:** `repo::merge_editor::gitlinks` (5), `ui::diff_view::submodule_note` (3)

<a id="view"></a>
## Graph, inspector and views

### Commit graph

#### Commit graph table (lanes, ref pills, columns) `view.graph-table`
- **Entry:** Center pane of every repo tab (ui/repo_view.rs -> graph_view::show); graph_view/mod.rs:224 column headers BRANCH / TAG, GRAPH, COMMIT MESSAGE, AUTHOR, DATE; lane colours: view.lane-colors
- **Platform:** `platform.design-system`, `platform.format`, `platform.git-model`, `platform.repo-load`
- **Own code:** `graph::layout::Graph::edges_in`, `graph::layout::Graph::index`, `ui::graph_view::columns`, `ui::graph_view::corner`, `ui::graph_view::rounded`
- **Shared:** `graph::layout::Graph::layout` (2), `graph::layout::Graph::reachable` (2), `graph::layout::free_lane` (2), `ui::graph_view::chip` (2), `ui::graph_view::pill_tip` (3), `ui::graph_view::ref_label` (3), `ui::graph_view::ref_pill` (4), `ui::graph_view::show` (17), `ui::graph_view::tip_now` (3)

#### Graph lane colours: ten distinct colours, merge lines in the merged branch's colour `view.lane-colors`
- **Entry:** graph lanes (src/graph/layout.rs:61 pick): a new lane takes the colour least used by the open lanes nearby and by the last ~60 rows, starting from its tip commit's seed colour; a merged-in branch never takes the merging lane's colour, so diverging branches differ; merge lines (src/graph/layout.rs:86 Graph::layout): a merge's line takes the merged branch's colour once its parent is placed; a merge into a branch that went on joins that branch's lane instead of running beside it; palettes src/theme/mod.rs:23 LANES (dark) and :37 LANES_LIGHT (light flavours): blue, pink, yellow, green, violet, red, teal, grey, orange, brown
- **Platform:** `platform.design-system`
- **Own code:** `graph::layout::pick`
- **Shared:** `graph::layout::free_lane` (2), `graph::layout::Graph::layout` (2), `theme::lane` (2), `theme::lane_of` (2), `ui::graph_view::show` (17)

#### Working-changes (WIP) row at the top of the graph `view.wip-row`
- **Entry:** graph_view/mod.rs row 0 "detached HEAD"/"HEAD" WIP row; click selects Sel::Wip
- **Platform:** `platform.repo-load`
- **Own code:** `graph::layout::Graph::with_wip`
- **Shared:** `ui::graph_view::show` (17)

#### Load more history on scroll (paging) `view.load-more`
- **Entry:** Scrolling to the end of the graph pushes Action::LoadMore (graph_view/mod.rs:637)
- **Platform:** `platform.repo-load`
- **Shared:** `graph::layout::Graph::has_unloaded_parent` (2), `ui::graph_view::show` (17)

#### First-parent-only graph toggle and "Viewing N of M refs / Show all" `view.first-parent`
- **Entry:** graph_view/mod.rs:167 "Show all" link; graph_view/mod.rs:246 first-parent toggle in header
- **Platform:** `platform.repo-load`
- **Shared:** `repo::data::View::change` (3), `ui::graph_view::show` (17)

#### Scroll to HEAD chip `view.head-chip`
- **Entry:** graph_view/mod.rs:652-668 HEAD chip "<branch> (HEAD)" with an up / down arrow when HEAD is scrolled out of view; click scrolls to it
- **Shared:** `ui::graph_view::chip` (2), `ui::graph_view::show` (17)

#### Select a commit / multi-select (click, Cmd/Shift-click) `view.select`
- **Entry:** Click a graph row; Cmd/Shift-click (graph_view/mod.rs:312)
- **Platform:** `platform.op-runner`
- **Shared:** `ui::graph_view::multi_select` (3), `ui::graph_view::show` (17)

#### Keyboard navigation in the graph (Up/Down, J/K) `view.keyboard-nav`
- **Entry:** graph_view/mod.rs:716 keyboard_nav
- **Own code:** `ui::graph_view::keyboard_nav`

#### Graph minimap (HEAD / selected / search-hit marks, click to jump) `view.minimap`
- **Entry:** Right edge of the graph (graph_view/minimap.rs)
- **Platform:** `platform.format`
- **Own code:** `ui::graph_view::minimap::minimap`

#### "Make your first commit" guide for an empty repository `view.empty-repo`
- **Entry:** Graph of a repo with no commits (graph_view/empty.rs)
- **Platform:** `platform.design-system`
- **Shared:** `ui::graph_view::empty::first_commit_guide` (2)

#### Hover a ref pill to highlight its commits / drag pills `view.ref-hover`
- **Entry:** graph pill hover/drag (graph_view/mod.rs, hover.rs, dnd.rs owned by branch area)
- **Shared:** `ui::graph_view::pill_tip` (3), `ui::graph_view::ref_pill` (4), `ui::graph_view::tip_now` (3)

### Search

#### Commit search bar (Cmd+F): text, author:, path:, code: queries `view.search`
- **Entry:** Cmd+F (ui/app.rs:265); graph_view/search.rs search bar; Enter/F3 next, Shift+Enter/Shift+F3 prev, Esc close, "Recent searches"
- **Platform:** `platform.git-runner`, `platform.task`
- **Own code:** `repo::search::Query::is_empty`, `repo::search::Search::dims`, `repo::search::Search::drop`, `repo::search::Search::is_ancestor`, `repo::search::Search::judge`, `repo::search::Search::open`, `repo::search::Search::poll`, `repo::search::Search::remember`, `repo::search::Search::running`, `repo::search::Search::start`, `repo::search::parse`, `repo::search::record`, `repo::search::tokens`
- **Shared:** `repo::search::Query::matches` (2), `repo::search::Search::filtered_data` (2), `repo::search::Search::rows` (2), `ui::graph_view::search::search_bar` (2)

#### Filter graph to search results / match case `view.search-filter`
- **Entry:** search bar toggles FUNNEL "Filter graph to results", "Aa" "Match case"
- **Platform:** `platform.repo-load`
- **Shared:** `repo::search::Query::matches` (2), `repo::search::Search::filtered_data` (2), `repo::search::Search::rows` (2), `ui::graph_view::search::search_bar` (2)

### Commit context menus (non-rewrite)

#### Copy SHA / message, Compare with HEAD / working tree `view.menu-copy-compare`
- **Entry:** Commit context menu (menus.rs:231-244) "Compare with HEAD", "Compare working tree with this commit", then "Copy commit SHA", "Copy commit message"
- **Platform:** `platform.op-runner`
- **Shared:** `ui::graph_view::menus::commit_menu` (24)

#### Checkout commit (detached) / ref, create branch / worktree / tag here `view.menu-checkout`
- **Entry:** menus.rs:145-187 "Checkout <ref>", "Checkout this commit (detached)", "Check out in another worktree", "Create branch here…", "Create worktree here (new branch)…", "Create tag here…", "Create annotated tag here…"
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `ui::graph_view::menus::checkout_action` (2), `ui::graph_view::menus::commit_menu` (24), `ui::graph_view::menus::on_branch` (3)

#### Merge / rebase / interactive rebase onto a ref or commit `view.menu-integrate`
- **Entry:** menus.rs:102 integrate_items: "Merge <ref> into <head>", "with options", "Rebase", "Interactive rebase…"; also sidebar and DnD
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `ui::graph_view::menus::integrate_items` (6), `ui::graph_view::menus::on_branch` (3)

#### Cherry-pick / revert a commit `view.menu-pick-revert`
- **Entry:** menus.rs:203-229 "Cherry-pick commit", "Cherry-pick without committing", "Revert commit"
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `ui::graph_view::menus::commit_menu` (24)

#### Reset current branch to commit (soft/mixed/hard) `view.menu-reset`
- **Entry:** menus.rs:303 reset_menu; also branch and tag menus
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `ui::graph_view::menus::reset_busy` (2), `ui::graph_view::menus::reset_items` (2), `ui::graph_view::menus::reset_menu` (2)

#### Move tag here `view.menu-move-tag`
- **Entry:** menus.rs:78 "Move tag here" submenu
- **Platform:** `platform.op-runner`
- **Shared:** `ui::graph_view::menus::move_tag_menu` (2)

#### Copy as patch / Save as patch `view.menu-patch`
- **Entry:** menus.rs:62 patch_items (commit menu :248, multi menu :37)
- **Platform:** `platform.op-runner`
- **Shared:** `ui::graph_view::menus::patch_items` (2)

#### Rewrite items in the commit menu (reword, split, squash, drop, amend, recompose, interactive rebase after) `view.menu-rewrite`
- **Entry:** menus.rs:262-299 "Edit history after this commit (interactive rebase)…", "Rewrite this commit" submenu, then reset_menu
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `ui::graph_view::menus::commit_menu` (24), `ui::graph_view::menus::recompose_items` (3)

#### Multi-commit context menu (cherry-pick, squash, revert, drop, recompose, compare ends, copy SHAs, bisect) `view.menu-multi`
- **Entry:** menus.rs:13 multi_menu on multi-selection
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `ui::graph_view::menus::multi_menu` (7), `ui::graph_view::menus::recompose_items` (3)

#### Working-changes row menu (Stash all, Discard all) `view.menu-wip`
- **Entry:** menus.rs:333 wip_menu
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `ui::graph_view::menus::discard_all_op` (2), `ui::graph_view::menus::wip_menu` (2)

### Inspector

#### Inspector panel dispatch and empty states `view.inspector-dispatch`
- **Entry:** Right panel (RepoTab::details): "Nothing to commit", "Select a commit"
- **Platform:** `platform.design-system`
- **Own code:** `ui::inspector::RepoTab::details`

#### Commit details panel (author, SHA copy, parents jump, message, files) `view.commit-details`
- **Entry:** Select a commit; commit.rs commit_panel; "Show more", copy SHA links
- **Platform:** `platform.design-system`, `platform.format`
- **Shared:** `ui::inspector::commit::RepoTab::commit_panel` (5), `ui::inspector::commit::avatar` (3), `ui::inspector::commit::linked` (2), `ui::inspector::commit::msg_editor` (4)

#### Compare two commits / commit vs HEAD / working tree `view.compare-details`
- **Entry:** Menu "Compare with HEAD", multi menu "Compare oldest <-> newest"; Sel::Compare in commit_panel
- **Platform:** `platform.repo-load`
- **Shared:** `ui::inspector::commit::RepoTab::commit_panel` (5)

#### Commit signature verification badge `view.signature-badge`
- **Entry:** commit.rs sig_badge (Good/Bad/Unknown signature)
- **Own code:** `ui::inspector::commit::sig_badge`

#### Edit commit message / remove co-author from the inspector `view.edit-message`
- **Entry:** commit.rs msg_editor "Save"/"Cancel", co-author remove button
- **Platform:** `platform.op-runner`
- **Shared:** `ui::inspector::commit::RepoTab::commit_panel` (5), `ui::inspector::commit::editable` (3), `ui::inspector::commit::msg_editor` (4)

#### Restore a file from a commit / its parent `view.restore-file`
- **Entry:** commit.rs restore_items file menu "Restore"
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `ui::inspector::commit::restore_items` (3)

#### Commit files as list or folder tree `view.file-tree`
- **Entry:** tree.rs:50 "View as a list" / "View as a folder tree"
- **Platform:** `platform.format`
- **Own code:** `repo::filetree::Dir::default`, `repo::filetree::FileTree::build`, `repo::filetree::FileTree::default`, `repo::filetree::FileTree::flatten`, `repo::filetree::FileTree::visible_files`
- **Shared:** `repo::filetree::FileTree::build_by` (2), `repo::filetree::FileTree::visible` (2), `repo::filetree::name_of` (2), `ui::inspector::tree::Order::place` (7), `ui::inspector::tree::dir_row` (8), `ui::inspector::tree::indent` (7), `ui::inspector::tree::list` (7), `ui::inspector::tree::show` (7), `ui::inspector::tree::toggle` (7), `ui::inspector::tree::visible` (9)

#### File row context menu (history, blame, open in editor, custom commands, permalinks, LFS track, copy path) `view.file-menus`
- **Entry:** inspector/mod.rs file_items/open_items/custom_items/permalink_items/lfs_track_item; file_row
- **Platform:** `platform.design-system`, `platform.op-runner`
- **Shared:** `ui::inspector::custom_items` (3), `ui::inspector::file_items` (2), `ui::inspector::file_row` (6), `ui::inspector::hovering` (5), `ui::inspector::lfs_track_item` (3), `ui::inspector::open_items` (4), `ui::inspector::permalink_items` (3), `ui::inspector::row_button` (5)

#### Multi-selection panel (cherry-pick, squash, revert, drop) `view.multi-panel`
- **Entry:** inspector/multi.rs multi_panel buttons
- **Platform:** `platform.op-runner`
- **Shared:** `ui::inspector::multi::RepoTab::multi_panel` (4)

### Diff view

#### Diff display: unified / split, scroll rail, change navigation (F7) `view.diff-display`
- **Entry:** Click a file; diff_view.rs view_controls "Unified"/"Split"; F7 / Shift+F7; click change rail
- **Platform:** `platform.design-system`, `platform.highlight`
- **Shared:** `repo::action::DiffSrc::change` (20), `ui::diff_view::Layout::get` (10), `ui::diff_view::change_starts` (2), `ui::diff_view::changed_span` (3), `ui::diff_view::minimap` (2), `ui::diff_view::paint_line` (11), `ui::diff_view::partners` (3), `ui::diff_view::row_changes` (10), `ui::diff_view::row_kind` (10), `ui::diff_view::rows` (10), `ui::diff_view::show` (20), `ui::diff_view::step` (2), `ui::diff_view::view_controls` (3), `ui::diff_view::wrap_before` (3), `ui::diff_view::wrap_group` (3)

#### Diff options: wrap, ignore whitespace, context size (3/10/File) `view.diff-options`
- **Entry:** view_controls "Wrap", whitespace toggle, "Context"
- **Platform:** `platform.git-runner`
- **Shared:** `repo::action::DiffSrc::change` (20), `ui::diff_view::show` (20), `ui::diff_view::view_controls` (3), `ui::diff_view::wrap_before` (3), `ui::diff_view::wrap_group` (3)

#### Word-level (changed span) highlighting in changed lines `view.diff-word`
- **Entry:** diff_view.rs partners/changed_span in split and unified
- **Platform:** `platform.highlight`
- **Shared:** `ui::diff_view::changed_span` (3), `ui::diff_view::paint_line` (11), `ui::diff_view::partners` (3)

#### Special diff notes: binary, submodule, LFS, large file, mode change, empty `view.diff-notes`
- **Entry:** diff_view.rs show/submodule_note/lfs_note/large_note
- **Platform:** `platform.format`
- **Shared:** `repo::action::DiffSrc::change` (20), `ui::diff_view::large_note` (2), `ui::diff_view::lfs_note` (3), `ui::diff_view::note_frame` (2), `ui::diff_view::show` (20), `ui::diff_view::submodule_note` (3)

#### Diff header and hunk/line actions (stage, unstage, discard, cherry-pick, revert lines, open/edit file, blame, history) `view.diff-actions`
- **Entry:** diff_view.rs buttons "Stage file", "Stage hunk", "Discard hunk", "Revert hunk", "Cherry-pick hunk", "Edit", "Blame", "File history", drag to pick lines
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::action::DiffSrc::change` (20), `ui::diff_view::apply_op` (9), `ui::diff_view::discard` (4), `ui::diff_view::line_menu` (2), `ui::diff_view::line_range` (2), `ui::diff_view::pick_lines` (10), `ui::diff_view::row_changes` (10), `ui::diff_view::show` (20), `ui::diff_view::stage` (4)

#### History of these lines (git log -L) `view.line-history`
- **Entry:** diff_view.rs:328 "History of N line(s)", line menu
- **Platform:** `platform.git-runner`
- **Own code:** `repo::history::line_history`, `repo::history::parse_lines`
- **Shared:** `repo::action::DiffSrc::change` (20), `ui::diff_view::line_menu` (2), `ui::diff_view::line_range` (2), `ui::diff_view::pick_lines` (10), `ui::diff_view::show` (20)

#### Image diff (swipe / side by side / before-after) `view.image-diff`
- **Entry:** Select an image file (diff_view.rs:410 -> ui/image_diff.rs)
- **Platform:** `platform.git-runner`, `platform.task`
- **Own code:** `repo::image_diff::is_image`, `repo::image_diff::load`, `repo::image_diff::load_side`, `repo::image_diff::sides`, `ui::image_diff::checker`, `ui::image_diff::column`, `ui::image_diff::dims`, `ui::image_diff::fit`, `ui::image_diff::picture`, `ui::image_diff::show`
- **Shared:** `repo::action::DiffSrc::change` (20), `ui::diff_view::show` (20)

### File history, blame, reflog, editor

#### File / folder history `view.file-history`
- **Entry:** File row menu "File history" / "Folder history"; diff header File history pill; Back/Forward (Cmd+[ / Cmd+])
- **Platform:** `platform.format`, `platform.git-runner`, `platform.task`
- **Own code:** `repo::history::FileView::more`, `repo::history::history`, `repo::history::parse_history`, `ui::history::hunk_height`, `ui::history::hunks`
- **Shared:** `repo::history::FileView::drop` (2), `repo::history::FileView::file_stamp` (2), `repo::history::FileView::go` (2), `repo::history::FileView::load` (2), `repo::history::FileView::open` (2), `repo::history::FileView::poll` (2), `repo::history::FileView::refresh` (2), `repo::history::FileView::step` (2), `repo::history::unquote` (2), `ui::history::show` (3)

#### Blame view with heat colors `view.blame`
- **Entry:** File row menu "Blame" / "Blame at HEAD"; history entry blame button
- **Platform:** `platform.git-runner`, `platform.highlight`, `platform.task`
- **Own code:** `repo::history::blame`, `repo::history::parse_blame`, `ui::history::blame_view`, `ui::history::heat_color`
- **Shared:** `repo::history::FileView::drop` (2), `repo::history::FileView::file_stamp` (2), `repo::history::FileView::go` (2), `repo::history::FileView::load` (2), `repo::history::FileView::open` (2), `repo::history::FileView::poll` (2), `repo::history::FileView::refresh` (2), `repo::history::FileView::step` (2), `repo::history::unquote` (2), `ui::history::show` (3)

#### Reflog browser (restore lost commits) `view.reflog`
- **Entry:** Toolbar history tools "Reflog — recover lost commits" (toolbar.rs:175), sidebar lifebuoy pill (sidebar/mod.rs:216), palette "Reflog — restore lost commits" (palette.rs:150)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.task`, `platform.undo-engine`
- **Own code:** `git::reflog::parse_reflog`, `git::reflog::reflog`, `repo::reflog::Reflog::open`, `repo::reflog::Reflog::poll`, `repo::reflog::Reflog::refresh`, `repo::reflog::reselect`, `ui::reflog::action_color`, `ui::reflog::entry_items`, `ui::reflog::show`

#### Built-in file editor with conflict-on-disk check `view.file-edit`
- **Entry:** Diff header "Edit" pill; Cmd+S saves, Esc cancels; unsaved-changes modal
- **Platform:** `platform.highlight`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::file_edit::FileEdit::check`, `repo::file_edit::FileEdit::crlf`, `repo::file_edit::FileEdit::dirty`, `repo::file_edit::FileEdit::load`, `repo::file_edit::FileEdit::on_disk`, `repo::file_edit::FileEdit::overwrite`, `repo::file_edit::FileEdit::save_op`, `repo::file_edit::contents`, `repo::file_edit::fingerprint`, `repo::file_edit::save`, `repo::file_edit::stat`, `repo::file_edit::text_key`, `ui::file_edit::show`

<a id="tools"></a>
## Stashes, worktrees, undo and tools

### Stashes

#### Stash all changes (with message) `tools.stash-all`
- **Entry:** src/ui/toolbar.rs:294 "Stash" pill; src/ui/graph_view/menus.rs:334 "Stash all changes" (WIP menu); src/ui/palette.rs:103 "Stash all changes…"; prompt dialog asks for optional message
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::op::stash_prompt`
- **Shared:** `repo::op::Op::new` (7), `repo::op::Op::revert` (5), `repo::op::exec_stash` (11), `repo::op::stash_cmd` (9), `ui::graph_view::menus::wip_menu` (2), `ui::palette::items` (25), `ui::toolbar::toolbar_local` (5)

#### Stash staged changes only `tools.stash-staged`
- **Entry:** src/ui/toolbar.rs:302 "Stash staged changes only"
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::op::exec_stash` (11), `repo::op::stash_cmd` (9), `ui::toolbar::toolbar_local` (5)

#### Stash selected file(s) `tools.stash-files`
- **Entry:** src/ui/inspector/wip.rs:17 "Stash this file" / "Stash N files" (WIP file context menu)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::op::Op::input` (2), `repo::op::exec_stash` (11), `repo::op::stash_cmd` (9), `repo::op::stash_paths_op` (2), `ui::inspector::wip::stash_files_item` (3)

#### Stashes sidebar section (list, select, multi-select, filter) `tools.stash-list`
- **Entry:** src/ui/sidebar/stashes.rs:14 STASHES section (click, ⌘/⇧-click multi-select, sidebar filter); stash markers in the graph src/ui/graph_view/mod.rs:509-544
- **Platform:** `platform.design-system`, `platform.git-model`, `platform.repo-load`
- **Shared:** `git::read::stash_info` (4), `ui::graph_view::show` (17), `ui::sidebar::stashes::list` (3)

#### Apply stash (keeps it; restores staged state when possible) `tools.stash-apply`
- **Entry:** src/ui/sidebar/stashes.rs:66 double-click a stash; src/ui/sidebar/stashes.rs:86 "Apply stash" menu; graph stash marker menu (graph_view/mod.rs:531, :544)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::op::apply_stash_op`
- **Shared:** `git::read::stash_info` (4), `repo::op::apply_stash` (3), `repo::op::exec_stash` (11), `repo::op::stash_cmd` (9), `repo::undo::stash_paths` (5), `ui::sidebar::stashes::list` (3), `ui::sidebar::stashes::stash_menu` (5)

#### Pop stash `tools.stash-pop`
- **Entry:** src/ui/sidebar/stashes.rs:89 "Pop stash"; src/ui/toolbar.rs:309 Pop stash pill/menu; src/ui/palette.rs:105 "Pop stash"; graph stash marker menu
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::op::apply_stash` (3), `repo::op::exec_stash` (11), `repo::op::pop_stash` (2), `repo::op::stash_cmd` (9), `repo::undo::stash_paths` (5), `ui::palette::items` (25), `ui::sidebar::stashes::stash_menu` (5), `ui::toolbar::toolbar_local` (5)

#### Rename stash `tools.stash-rename`
- **Entry:** src/ui/sidebar/stashes.rs:97 "Rename stash…"
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::op::exec_stash` (11), `repo::op::rename_stash` (2), `repo::op::stash_cmd` (9), `ui::sidebar::stashes::stash_menu` (5)

#### Create branch from stash `tools.stash-branch`
- **Entry:** src/ui/sidebar/stashes.rs:92 "Create branch from stash…"
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::op::stash_branch`
- **Shared:** `repo::op::apply_stash` (3), `repo::op::exec_stash` (11), `repo::op::stash_cmd` (9), `repo::undo::stash_paths` (5), `ui::sidebar::stashes::stash_menu` (5)

#### Delete stash(es) (undoable) `tools.stash-delete`
- **Entry:** src/ui/sidebar/stashes.rs:100 "Delete stash"; stashes.rs:72 "Delete N stashes…" (multi-selection menu)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `git::read::stash_info` (4), `repo::op::drop_stashes` (2), `repo::op::exec_stash` (11), `repo::op::stash_cmd` (9), `ui::sidebar::stashes::list` (3), `ui::sidebar::stashes::stash_menu` (5)

#### Undo of stash save / pop / rename / delete (⌘Z) `tools.stash-undo`
- **Entry:** ⌘Z / toolbar Undo after any stash operation
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::op::drop_stashes` (2), `repo::op::exec_stash` (11), `repo::op::pop_stash` (2), `repo::op::rename_stash` (2), `repo::op::stash_cmd` (9), `repo::undo::stash_paths` (5)

#### Apply one file from a stash `tools.stash-apply-file`
- **Entry:** src/ui/inspector/commit.rs:350 "Apply this file" (file context menu while a stash is selected)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::restore::apply_stash`, `repo::restore::apply_stash_file`
- **Shared:** `repo::restore::restore_file` (3), `ui::inspector::commit::RepoTab::commit_panel` (5)

### Worktrees

#### Worktrees sidebar section (rows read folder · branch, primary worktree marked) `tools.worktree-list`
- **Entry:** src/ui/sidebar/worktrees.rs:9 WORKTREES section; each row (worktrees.rs:44 worktree_row) reads "<folder> · <branch>" (:37 worktree_name: "primary" for the repository's own folder); check = this tab, house = primary, tree = linked; change count, lock, prunable rows struck through; tooltip with the path ("Primary worktree (the repository's own folder)")
- **Platform:** `platform.design-system`, `platform.repo-load`
- **Shared:** `git::read::worktrees` (5), `ui::sidebar::worktrees::list` (3), `ui::sidebar::worktrees::worktree_row` (6), `ui::sidebar::worktrees::worktree_name` (2)

#### Create a worktree (new branch, from HEAD / commit / branch / remote branch) `tools.worktree-create`
- **Entry:** src/ui/sidebar/worktrees.rs:20 "New worktree…"; src/ui/graph_view/menus.rs:177 "Create worktree here (new branch)…"; src/ui/sidebar/ref_menus.rs:271 "Open in a new worktree…"; src/ui/sidebar/ref_menus.rs:308 "Check out in a new worktree…"
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `git::read::worktrees` (5), `repo::data::RepoData::worktree_path` (3), `repo::op::Op::then_open` (4), `ui::app::App::poll_tabs` (8), `ui::app::App::switch` (4), `ui::graph_view::menus::commit_menu` (24), `ui::sidebar::ref_menus::remote_menu` (9), `ui::sidebar::worktrees::list` (3), `ui::sidebar::ref_menus::worktree_items` (6)

#### Open / switch to a worktree in place `tools.worktree-switch`
- **Entry:** src/ui/sidebar/worktrees.rs:68 click or double-click a worktree row; src/ui/sidebar/ref_menus.rs:276 "Open its worktree"; branch checkout of a branch living in another worktree (op::branch_checkout); after a worktree is created (op.then_open)
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::RepoTab::apply` (21), `repo::op::RepoTab::draft_message` (5), `repo::op::branch_checkout` (7), `repo::undo::RepoTab::take_steps` (2), `ui::app::App::poll_tabs` (8), `ui::app::App::switch` (4), `ui::app::draft` (5), `ui::app::draft_file` (3), `ui::app::drop_data_off_thread` (3), `ui::app::forget_draft` (5), `ui::app::replaceable` (2), `ui::sidebar::ref_menus::local_menu` (22), `ui::sidebar::worktrees::worktree_row` (6), `ui::sidebar::ref_menus::worktree_items` (6)

#### Open a worktree in a new tab `tools.worktree-open-tab`
- **Entry:** src/ui/sidebar/worktrees.rs:71 "Open in new tab"
- **Platform:** `platform.op-runner`
- **Shared:** `repo::RepoTab::apply` (21), `ui::app::App::open` (5), `ui::app::App::poll_tabs` (8), `ui::sidebar::worktrees::worktree_row` (6)

#### Lock / unlock a worktree `tools.worktree-lock`
- **Entry:** src/ui/sidebar/worktrees.rs:88 "Lock…" / "Unlock"
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `ui::sidebar::worktrees::worktree_row` (6)

#### Remove a worktree (optionally delete its branch) `tools.worktree-remove`
- **Entry:** src/ui/sidebar/worktrees.rs:96 "Remove worktree" / "Remove worktree and delete <branch>"; Windows: a worktree folder still in use is retried for 5 s before the removal fails (src/repo/op.rs:201 worktree_remove)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::op::Op::confirm` (4), `repo::op::Op::then` (2), `repo::op::branch_cmd` (3), `repo::op::delete_branches` (4), `repo::op::remove_worktree_and_branch` (2), `repo::op::remove_worktree_op` (3), `repo::op::removes_worktree` (3), `repo::op::worktree_remove` (3), `repo::op::worktree_path` (3), `repo::op::worktree_remove_args` (4), `ui::sidebar::worktrees::worktree_row` (6)

#### Remove the worktree this tab shows (keep its branch; the tab moves to the primary worktree) `tools.worktree-remove-this`
- **Entry:** HEAD branch's menu in the sidebar or on its graph pill: "Remove this worktree (keep <branch>)" (src/ui/sidebar/ref_menus.rs:287 worktree_items), only in a linked, unlocked worktree; confirms first; runs git -C <primary worktree> worktree remove (src/git/runner.rs:253 runs a -C command from that folder); this tab's file watcher stops first (src/repo/op.rs:1146) and restarts if the removal fails (op.rs:1245); on success the tab switches to the primary worktree (then_open); Windows: a folder still in use is retried for 5 s (src/repo/op.rs:201 worktree_remove)
- **Platform:** `platform.op-runner`, `platform.watcher`, `platform.git-runner`
- **Shared:** `ui::sidebar::ref_menus::worktree_items` (6), `repo::op::remove_worktree_op` (3), `repo::op::worktree_remove_args` (4), `repo::op::removes_worktree` (3), `repo::op::worktree_remove` (3), `repo::op::worktree_path` (3), `repo::op::exec` (7), `repo::op::RepoTab::run` (5), `repo::op::RepoTab::poll_op` (2), `repo::RepoTab::start_watching` (2), `git::runner::exec_both` (2), `repo::op::Op::then_open` (4), `ui::app::App::poll_tabs` (8), `ui::app::App::switch` (4)

#### Prune stale worktrees `tools.worktree-prune`
- **Entry:** src/ui/sidebar/worktrees.rs:27 "Prune stale"
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `git::read::worktrees` (5), `ui::sidebar::worktrees::list` (3)

#### Worktree row: show in file manager, copy path `tools.worktree-misc`
- **Entry:** src/ui/sidebar/worktrees.rs:74 "Show in file manager"; worktrees.rs:77 "Copy path"
- **Shared:** `repo::files::reveal` (3), `ui::sidebar::worktrees::worktree_row` (6)

#### Check out a commit in another worktree without switching to it `tools.worktree-checkout-commit`
- **Entry:** src/ui/graph_view/menus.rs commit_menu "Check out in another worktree ▸ <worktree>"
- **Platform:** `platform.op-runner`, `platform.git-runner`, `platform.repo-load`
- **Own code:** `repo::op::checkout_in`
- **Shared:** `git::read::worktrees` (5), `repo::data::RepoData::count_worktree_changes` (2), `repo::op::exec` (7), `ui::graph_view::menus::commit_menu` (24)

### Undo, redo and restore

#### Undo / Redo (⌘Z, ⇧⌘Z, toolbar, palette) `tools.undo-redo-ui`
- **Entry:** ⌘Z / ⇧⌘Z / ⌘Y src/ui/app.rs:259-264; toolbar Undo/Redo pills src/ui/toolbar.rs:91-97 (labelled with the step); ⌘P "Undo <label>" / "Redo <label>" src/ui/palette.rs:129, :132
- **Platform:** `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::RepoTab::apply` (21), `repo::undo::RepoTab::redo_label` (2), `repo::undo::RepoTab::undo_label` (2), `repo::undo::RepoTab::undo_top` (3), `ui::app::App::shortcuts` (11), `ui::palette::items` (25), `ui::toolbar::toolbar` (8)

#### Toast with Undo button `tools.toast-undo`
- **Entry:** src/ui/toasts.rs:55 Undo button on the toast of the newest undoable step
- **Platform:** `platform.design-system`, `platform.undo-engine`
- **Shared:** `repo::undo::RepoTab::undo_top` (3), `ui::app::App::poll_tabs` (8), `ui::toasts::App::show_toasts` (2), `ui::toasts::App::toast` (2), `ui::toasts::App::toast_undo` (2)

#### Restore a file from a commit / as before the commit `tools.restore-file`
- **Entry:** src/ui/inspector/commit.rs:82 "Restore this file from this commit" and "Restore as it was before this commit" (file context menu)
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::restore::restore_file` (3), `ui::inspector::commit::RepoTab::commit_panel` (5), `ui::inspector::commit::restore_items` (3)

### Patches

#### Copy / save commits as patch `tools.patch-export`
- **Entry:** src/ui/graph_view/menus.rs:64 "Copy as patch" / "Save as patch…" (commit and multi-select menus)
- **Platform:** `platform.git-runner`, `platform.task`
- **Own code:** `repo::patch::RepoTab::export_patch`, `repo::patch::RepoTab::poll_patch`, `repo::patch::format_patch`
- **Shared:** `ui::graph_view::menus::patch_items` (2)

#### Apply patch file (git am / git apply) `tools.patch-apply`
- **Entry:** src/ui/toolbar.rs:183 "Apply patch…" (History tools menu); src/ui/palette.rs:168 "Apply patch…"
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::patch::RepoTab::apply_patch`, `repo::patch::apply_op`
- **Shared:** `ui::palette::items` (25), `ui::toolbar::history_tools` (7)

### Terminal

#### Embedded terminal panel (toggle, tabs, new, close, busy guard) `tools.terminal-panel`
- **Entry:** ⌃` src/ui/app.rs:199-203; toolbar terminal pill src/ui/toolbar.rs:26; ⌘P "Toggle terminal panel" src/ui/palette.rs:137; panel src/ui/repo_view.rs:46-53 ("New terminal", tab buttons, "Stop & close", "Keep", hide); Settings → Built-in terminal
- **Platform:** `platform.design-system`
- **Own code:** `repo::terminal::Listener::send_event`, `repo::terminal::Session::drop`, `repo::terminal::Session::poll`, `repo::terminal::Session::spawn`, `repo::terminal::Terminals::add`, `repo::terminal::Terminals::close`, `repo::terminal::Terminals::fix_after_removal`, `repo::terminal::Terminals::keep_busy`, `repo::terminal::Terminals::poll`, `repo::terminal::Terminals::toggle`, `repo::terminal::created`, `ui::terminal::id`, `ui::terminal::show`
- **Shared:** `repo::terminal::Terminals::busy` (2), `repo::terminal::Terminals::running` (2), `ui::app::App::shortcuts` (11), `ui::palette::items` (25), `ui::repo_view::show` (6), `ui::toolbar::toolbar` (8)

#### Terminal grid rendering, selection and scrolling `tools.terminal-render`
- **Entry:** src/ui/terminal.rs:78 grid (cells, cursor, selection, wheel, IME)
- **Platform:** `platform.design-system`
- **Own code:** `repo::terminal::Session::resize`, `ui::terminal::grid`, `ui::terminal::rgb`
- **Shared:** `repo::terminal::Session::write` (2)

#### Terminal keyboard input, paste and copy `tools.terminal-input`
- **Entry:** src/ui/terminal.rs:260 input (keys, text, paste, copy/cut, Alt as Meta off macOS)
- **Own code:** `ui::terminal::alt_as_meta`, `ui::terminal::input`, `ui::terminal::key_bytes`, `ui::terminal::paste`
- **Shared:** `repo::terminal::Session::write` (2)

#### Warn before closing a tab or quitting while a terminal command runs `tools.terminal-busy-guard`
- **Entry:** src/ui/app.rs:293 close_tab; src/ui/app.rs:364 guard_close / quit_dialog
- **Own code:** `repo::terminal::Session::busy`, `repo::terminal::has_child`
- **Shared:** `repo::terminal::Terminals::busy` (2), `repo::terminal::Terminals::running` (2), `ui::app::App::close_tab` (5), `ui::app::App::guard_close` (4), `ui::app::App::quit_dialog` (3)

### External tools and custom commands

#### Open repo / file in external editor `tools.open-editor`
- **Entry:** src/ui/toolbar.rs:163 "Open in editor"; file menus "Open in editor" src/ui/inspector/mod.rs:67; diff header code pill src/ui/diff_view.rs:264; ⌘P Open in editor
- **Own code:** `repo::tools::default_editor`, `repo::tools::gui_editor`, `repo::tools::open_editor`
- **Shared:** `repo::RepoTab::apply` (21), `repo::action::DiffSrc::change` (20), `repo::tools::launch` (2), `repo::tools::launch_path` (2), `repo::tools::launch_words` (2), `repo::tools::which` (2), `repo::tools::words` (2), `ui::diff_view::show` (20), `ui::inspector::open_items` (4), `ui::toolbar::open_items` (4)

#### Open external terminal here `tools.open-terminal`
- **Entry:** src/ui/toolbar.rs:164 "Open terminal here"; ⌃` when Settings → Built-in terminal is off (src/ui/app.rs:203)
- **Own code:** `repo::tools::default_terminal`, `repo::tools::open_terminal`
- **Shared:** `repo::RepoTab::apply` (21), `repo::tools::launch` (2), `repo::tools::launch_path` (2), `repo::tools::launch_words` (2), `repo::tools::which` (2), `repo::tools::words` (2), `ui::app::App::shortcuts` (11), `ui::toolbar::open_items` (4)

#### Show in file manager `tools.reveal`
- **Entry:** src/ui/toolbar.rs:165 "Show in file manager"; tab context menu src/ui/titlebar.rs:87 "Reveal in file manager"; WIP conflict menu src/ui/inspector/wip.rs:543; worktree row menu
- **Shared:** `repo::RepoTab::apply` (21), `repo::files::reveal` (3), `ui::inspector::wip::RepoTab::wip_panel` (31), `ui::titlebar::App::tab_bar` (12), `ui::toolbar::open_items` (4)

#### Custom commands (Settings → Tools; {repo} {branch} {sha} {path}) `tools.custom-commands`
- **Entry:** Settings → Tools src/ui/settings.rs:419; ⌘P custom commands src/ui/palette.rs:146; commit menu src/ui/graph_view/menus.rs:256; file context menus src/ui/inspector/mod.rs:75
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::tools::Custom::uses`, `repo::tools::commands`, `repo::tools::fill`, `repo::tools::op`, `repo::tools::quote`, `repo::tools::quoted_key`, `repo::tools::set`
- **Shared:** `repo::RepoTab::apply` (21), `ui::app::App::ui` (10), `ui::graph_view::menus::commit_menu` (24), `ui::inspector::custom_items` (3), `ui::inspector::open_items` (4), `ui::palette::items` (25), `ui::settings::tools_section` (2)

#### External merge tool for a conflicted file `tools.merge-tool`
- **Entry:** src/ui/inspector/wip.rs:537 "Open in external merge tool"; Cancel at wip.rs:80 open_tools; "Restore conflict markers" wip.rs:547
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Shared:** `repo::tools::RepoTab::close_tool` (2), `repo::tools::RepoTab::merge_tool_open` (3), `repo::tools::RepoTab::open_tool` (3), `repo::tools::RepoTab::poll_tools` (3), `repo::tools::RepoTab::tool_blocks` (2), `repo::tools::kind` (3), `repo::tools::mergetool_op` (3), `repo::tools::restore_markers_op` (3), `repo::tools::tool` (3), `repo::tools::touches` (2), `ui::dialogs::ai_dialog` (4), `ui::inspector::wip::RepoTab::wip_panel` (31), `ui::inspector::wip::open_tools` (5), `ui::repo_view::show` (6)

#### External diff tool for a file `tools.diff-tool`
- **Entry:** src/ui/diff_view.rs:270 diff header pill
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::tools::difftool_op`
- **Shared:** `repo::action::DiffSrc::change` (20), `repo::tools::RepoTab::open_tool` (3), `repo::tools::RepoTab::poll_tools` (3), `repo::tools::kind` (3), `repo::tools::tool` (3), `ui::diff_view::show` (20)

#### Quit/close guard while external tools are open `tools.tool-guard`
- **Entry:** src/ui/app.rs:303 close_tab, :373 guard_close, :402 quit_dialog
- **Shared:** `ui::app::App::close_tab` (5), `ui::app::App::guard_close` (4), `ui::app::App::quit_dialog` (3)

### Maintenance and logs

#### Perform repo maintenance `tools.maintenance-run`
- **Entry:** src/ui/toolbar.rs:194 "Perform repo maintenance"; ⌘P "Perform repo maintenance"; optimize hint src/ui/repo_view.rs:161 "Optimize"
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::maintenance::op`
- **Shared:** `repo::maintenance::run` (2), `ui::palette::items` (25), `ui::repo_view::optimize_hint` (3), `ui::toolbar::history_tools` (7)

#### Enable / disable background maintenance `tools.maintenance-schedule`
- **Entry:** src/ui/toolbar.rs:200 "Enable background maintenance" / "Disable background maintenance" (History tools menu); ⌘P "Enable background maintenance" / "Disable background maintenance"
- **Platform:** `platform.git-runner`, `platform.op-runner`, `platform.undo-engine`
- **Own code:** `repo::maintenance::schedule_op`
- **Shared:** `repo::maintenance::run` (2), `ui::palette::items` (25), `ui::toolbar::history_tools` (7)

#### Optimize repository hint strip `tools.optimize-hint`
- **Entry:** src/ui/repo_view.rs:149 strip above graph (loose objects / no commit-graph)
- **Platform:** `platform.repo-load`
- **Own code:** `repo::maintenance::Health::wants_optimizing`, `repo::maintenance::health`
- **Shared:** `ui::repo_view::optimize_hint` (3)

#### Command log panel `tools.command-log`
- **Entry:** titlebar button src/ui/titlebar.rs:25; ⇧⌘L src/ui/app.rs:219; ⌘P command log; panel src/ui/app.rs:570
- **Platform:** `platform.design-system`, `platform.git-runner`
- **Own code:** `ui::command_log::show`
- **Shared:** `ui::app::App::shortcuts` (11), `ui::app::App::ui` (10)

<a id="platform"></a>
## Platform

### Platform

#### Git process runner (run/exec/stream/cancel/env, credential redaction) `platform.git-runner`
- **Entry:** every git call; GLASIR_PERF logs slow ones
- **Used by:** 120 features
- **Own code:** `git::runner::Cancel::active`, `git::runner::Cancel::attach`, `git::runner::Cancel::cancel`, `git::runner::Cancel::detach`, `git::runner::Cancel::is_cancelled`, `git::runner::Cancel::progress`, `git::runner::Cancel::scope`, `git::runner::base`, `git::runner::own_group`, `git::runner::kill_tree`, `git::runner::cmd`, `git::runner::exec`, `git::runner::last_progress`, `git::runner::progress_label`, `git::runner::read_all`, `git::runner::read_small`, `git::runner::run`, `git::runner::run_capped`, `git::runner::run_input`, `git::runner::run_raw`, `git::runner::run_shell`, `git::runner::run_stderr`, `git::runner::run_tool`, `git::runner::spawn`, `git::runner::spawning`, `git::runner::squash_progress`, `git::runner::stream`
- **Shared:** `git::env::check_git` (3), `git::env::import_login_env` (3), `git::env::parse_env` (3), `git::env::parse_version` (3), `git::runner::exec_both` (2), `git::url::redact` (6), `git::url::redact_command` (3), `git::url::secrets` (3), `git::url::shell_words` (3)

#### Op / Action dispatch and execution `platform.op-runner`
- **Entry:** RepoTab::apply for every Action; RepoTab::run for every Op; poll_op after the thread finishes
- **Used by:** 176 features
- **Platform:** `platform.git-runner`, `platform.offer`, `platform.task`, `platform.undo-engine`
- **Own code:** `repo::RepoTab::cancellable`, `repo::RepoTab::progress`, `repo::action::op`, `repo::op::Dropped::from`, `repo::op::Op::backup`, `repo::op::Op::light`, `repo::op::Undo::mode`, `repo::op::network`, `repo::op::own_state`, `repo::op::seq_files`, `repo::op::seq_state`, `repo::op::state_files`
- **Shared:** `repo::RepoTab::apply` (21), `repo::RepoTab::busy` (2), `repo::RepoTab::prepare` (5), `repo::RepoTab::unpark_rebase` (2), `repo::op::Op::confirm` (4), `repo::op::Op::input` (2), `repo::op::Op::new` (7), `repo::op::Op::revert` (5), `repo::op::Op::then` (2), `repo::op::Op::then_open` (4), `repo::op::Op::undo` (4), `repo::op::RepoTab::poll_op` (2), `repo::op::RepoTab::run` (5), `repo::op::exec` (7), `repo::op::exec_stash` (11), `repo::op::in_progress` (4), `repo::op::switch_refused` (3), `repo::op::switches_head` (3), `repo::op::without_vanished` (6)

#### Undo / redo engine and discard backups `platform.undo-engine`
- **Entry:** ⌘Z / ⇧⌘Z, toolbar Undo/Redo, toast undo
- **Used by:** 107 features
- **Platform:** `platform.git-runner`
- **Own code:** `git::snapshot::ancestors`, `git::snapshot::blobs`, `git::snapshot::clean_snap`, `git::snapshot::commit`, `git::snapshot::current`, `git::snapshot::drop_backups`, `git::snapshot::empty_tree`, `git::snapshot::flagged`, `git::snapshot::gone`, `git::snapshot::hash`, `git::snapshot::hash_paths`, `git::snapshot::head`, `git::snapshot::icase`, `git::snapshot::index_tree`, `git::snapshot::info`, `git::snapshot::keep`, `git::snapshot::kept`, `git::snapshot::key`, `git::snapshot::leaves`, `git::snapshot::lstat`, `git::snapshot::move_files`, `git::snapshot::move_refs`, `git::snapshot::on_index`, `git::snapshot::prune`, `git::snapshot::put_raw`, `git::snapshot::raw`, `git::snapshot::related`, `git::snapshot::running`, `git::snapshot::scratch`, `git::snapshot::snap`, `git::snapshot::snap_in`, `git::snapshot::snapshot`, `git::snapshot::tree`, `git::snapshot::used_elsewhere`, `git::snapshot::view`, `git::snapshot::write`, `git::snapshot::z`, `repo::undo::RepoTab::clear_redo`, `repo::undo::RepoTab::put_steps`, `repo::undo::RepoTab::retry_or_skip`, `repo::undo::RepoTab::undo_redo`, `repo::undo::RepoTab::undone`, `repo::undo::UndoEntry::new`, `repo::undo::at`, `repo::undo::backed_up`, `repo::undo::backup`, `repo::undo::copy`, `repo::undo::delta`, `repo::undo::dirty`, `repo::undo::eq`, `repo::undo::halves`, `repo::undo::moved`, `repo::undo::on_remotes`, `repo::undo::own`, `repo::undo::plan`, `repo::undo::recursed`, `repo::undo::scope`, `repo::undo::split_z`, `repo::undo::tip`, `repo::undo::transition`, `repo::undo::unchanged`
- **Shared:** `git::read::parse_worktrees` (2), `repo::undo::RepoTab::redo_label` (2), `repo::undo::RepoTab::take_steps` (2), `repo::undo::RepoTab::undo_label` (2), `repo::undo::RepoTab::undo_top` (3), `repo::undo::stash_paths` (5)

#### Repository load (data, log, refs, status) and command log storage `platform.repo-load`
- **Entry:** tab open, reload (F5, focus, watcher, after ops), commit selection details; command log window reads git::log::LOG
- **Used by:** 32 features
- **Platform:** `platform.git-model`, `platform.git-runner`, `platform.task`
- **Own code:** `git::log::Logged::shell`, `git::log::record`, `git::log::shell_quote`, `git::read::author`, `git::read::commit_info`, `git::read::compare_info`, `git::read::config`, `git::read::git_dir`, `git::read::log`, `git::read::numstat`, `git::read::parse_config`, `git::read::parse_ident`, `git::read::parse_log`, `git::read::parse_name_status`, `git::read::parse_numstat`, `git::read::parse_status`, `git::read::sig_code`, `git::read::stashes`, `git::read::status`, `repo::RepoTab::compare_query`, `repo::RepoTab::load_info`, `repo::RepoTab::load_info_soon`, `repo::RepoTab::loading`, `repo::RepoTab::locate`, `repo::RepoTab::new`, `repo::RepoTab::picked_lately`, `repo::RepoTab::select`, `repo::RepoTab::shown`, `repo::data::RepoData::build`, `repo::data::RepoData::build_reusing`, `repo::data::RepoData::commit`, `repo::data::RepoData::first_parent`, `repo::data::RepoData::rows`, `repo::data::View::filtered`, `repo::data::is_trunk`, `repo::data::is_trunk_name`, `repo::data::load_all`
- **Shared:** `git::read::conflict_kind` (5), `git::read::parse_worktrees` (2), `git::read::refs` (2), `git::read::remotes` (3), `git::read::stash_info` (4), `git::read::state` (3), `git::read::worktrees` (5), `repo::RepoTab::poll` (3), `repo::RepoTab::reload` (3), `repo::data::RepoData::branch_rank` (3), `repo::data::RepoData::cfg` (3), `repo::data::RepoData::cfg_all` (2), `repo::data::RepoData::cfg_bool` (2), `repo::data::RepoData::count_worktree_changes` (2), `repo::data::RepoData::default_branch` (4), `repo::data::RepoData::default_remote` (8), `repo::data::RepoData::load_bisect` (2), `repo::data::RepoData::load_conflicts` (2), `repo::data::RepoData::local` (7), `repo::data::RepoData::on_remote` (2), `repo::data::RepoData::on_remote_in_graph` (2), `repo::data::RepoData::push_dest` (3), `repo::data::RepoData::push_target` (4), `repo::data::RepoData::reachable` (3), `repo::data::RepoData::rev` (3), `repo::data::RepoData::shows` (2), `repo::data::RepoData::split_remote` (13), `repo::data::RepoData::web` (4), `repo::data::RepoData::web_of` (4), `repo::data::RepoData::webs` (2), `repo::data::RepoData::worktree_path` (3), `repo::data::RepoData::worktree_rev` (2), `repo::data::View::revs` (3), `repo::data::split_remote` (2)

#### File watcher (outside changes reload the repo) `platform.watcher`
- **Entry:** background: notify events in the worktree and .git
- **Used by:** 4 features
- **Platform:** `platform.task`
- **Own code:** `repo::RepoTab::watching`, `repo::watch::classify`, `repo::watch::is_ignored`, `repo::watch::watch`
- **Shared:** `repo::RepoTab::start_watching` (2)

#### Background Task (thread + channel) `platform.task`
- **Entry:** every off-UI-thread job
- **Used by:** 36 features
- **Own code:** `repo::task::Task::poll`, `repo::task::Task::spawn`

#### Liquid Glass design system (colors, flavours, fonts, glass widgets) `platform.design-system`
- **Entry:** every screen
- **Used by:** 41 features
- **Own code:** `theme::Flavor::dark`, `theme::Flavor::name`, `theme::Flavor::palette`, `theme::Flavor::raw_palette`, `theme::accent`, `theme::add`, `theme::added_bg`, `theme::apply`, `theme::bg`, `theme::change_color`, `theme::change_colors`, `theme::changed`, `theme::contrast`, `theme::dark`, `theme::del`, `theme::dim`, `theme::faint`, `theme::flavor`, `theme::glass::bar`, `theme::glass::bar_h`, `theme::glass::canvas`, `theme::glass::canvas_in`, `theme::glass::card`, `theme::glass::context_menu`, `theme::glass::empty_state`, `theme::glass::glass`, `theme::glass::glass_shapes`, `theme::glass::group`, `theme::glass::inside_rounded`, `theme::glass::keyed`, `theme::glass::photo`, `theme::glass::pill`, `theme::glass::progress`, `theme::glass::radial`, `theme::glass::rest`, `theme::glass::see_through`, `theme::glass::set_transparency`, `theme::glass::slider`, `theme::glass::transparency`, `theme::hover`, `theme::install_fonts`, `theme::luminance`, `theme::on_color`, `theme::removed_bg`, `theme::selection`, `theme::system_fonts`, `theme::tag`, `theme::text`, `theme::visuals`
- **Shared:** `theme::avatar` (2), `theme::glass::description` (2), `theme::glass::primary` (2), `theme::glass::small` (2), `theme::lane` (2), `theme::lane_of` (2)

#### AI client: provider config, token storage, Claude / OpenAI API streaming, Claude Code / Codex CLI runs, Job plumbing `platform.ai-client`
- **Entry:** every AI feature; prompt building (Purpose -> build) is one match in ai::tasks::build and is not separable per feature, so it stays here; ai::client::complete dispatches to the Claude API, the OpenAI API or a CLI (ai/cli.rs)
- **Used by:** 10 features
- **Platform:** `platform.git-runner`, `platform.task`
- **Own code:** `ai::client::Endpoint::default`, `ai::client::Provider::account`, `ai::client::env_source`, `ai::client::find_saved`, `ai::client::find_saved_cached`, `ai::client::keychain::delete`, `ai::client::keychain::get`, `ai::client::keychain::set`, `ai::client::keychain_token`, `ai::client::plain_path`, `ai::client::plain_token`, `ai::client::post`, `ai::client::remember`, `ai::client::remove_plain_token`, `ai::client::save_plain_token`, `ai::client::save_token`, `ai::client::saved`, `ai::client::supports_effort`, `ai::client::tls`, `ai::client::token`, `ai::enabled`, `ai::openai::complete`, `ai::tasks::head_or_empty`, `ai::tasks::stash_state`
- **Shared:** `ai::client::Config::current` (2), `ai::client::Config::model` (2), `ai::client::Provider::is_cli` (2), `ai::cli::endpoint` (2), `ai::cli::complete` (2), `ai::cli::spawn` (2), `ai::cli::feed` (2), `ai::cli::stderr_of` (2), `ai::cli::private_dir` (2), `ai::cli::read` (2), `ai::cli::stop` (2), `ai::cli::failed` (2), `ai::cli::claude` (2), `ai::cli::codex` (2), `ai::client::Config::default` (2), `ai::client::Provider::label` (2), `ai::client::Provider::models` (2), `ai::client::Provider::name` (2), `ai::client::complete` (2), `ai::client::env_token` (2), `ai::client::need_token` (2), `ai::name` (2), `ai::sync` (2), `ai::tasks::Job::poll` (3), `ai::tasks::Job::running` (3), `ai::tasks::Purpose::title` (3), `ai::tasks::build` (5), `ai::tasks::compose_schema` (3), `ai::tasks::diff_with_stat` (2), `ai::tasks::parent_or_empty` (2), `ai::tasks::recent_subjects` (2), `ai::tasks::repo_instructions` (2), `ai::tasks::secret` (2), `ai::tasks::start` (5), `ai::tasks::system_with` (2), `ai::tasks::too_large` (2), `ai::tasks::uncommitted_changes` (2)

#### Syntax highlighting of diff lines `platform.highlight`
- **Entry:** diff view, history/blame
- **Used by:** 5 features
- **Own code:** `highlight::display`, `highlight::display_len`, `highlight::highlight`, `highlight::text`

#### UI helpers (relative time, key labels, focus/Esc handling, text jobs) `platform.format`
- **Entry:** used across ui/
- **Used by:** 11 features
- **Own code:** `ui::format::age`, `ui::format::ago`, `ui::format::bytes`, `ui::format::change_summary`, `ui::format::dismissed`, `ui::format::dismissed_in`, `ui::format::esc_pressed`, `ui::format::esc_pressed_in`, `ui::format::focused_last_frame`, `ui::format::initials`, `ui::format::keys`, `ui::format::lead_top`, `ui::format::modal_open`, `ui::format::note_focus`, `ui::format::nothing_focused_here`, `ui::format::nothing_was_focused`, `ui::format::on_top`, `ui::format::pc_keys`, `ui::format::plural`, `ui::format::relative_time`, `ui::format::text_job`, `ui::format::tops`, `ui::format::view_key`, `ui::format::visible`, `ui::format::wrap_job`, `ui::format::wrapped`

#### Failure -> choice mapping `platform.offer`
- **Entry:** failed ops show choice dialog (pull/push/cherry-pick/checkout/signing)
- **Used by:** 24 features
- **Own code:** `repo::offer::cherry_pick_anyway`
- **Shared:** `repo::offer::autostash_checkout` (3), `repo::offer::offer` (2), `repo::offer::pull_choices` (2), `repo::offer::push_rejected` (3), `repo::offer::signing_error` (2), `repo::offer::signing_failed` (2)

#### Git data model helpers `platform.git-model`
- **Entry:** Commit/Ref/Status types
- **Used by:** 10 features
- **Own code:** `git::model::Status::changed_count`, `git::model::Status::is_dirty`
- **Shared:** `git::short_ref` (2)
