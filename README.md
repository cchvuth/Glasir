<p align="center"><img src="assets/icon.png" width="160" alt="Glasir icon: a glass leaf whose veins are a commit graph, with golden commits"></p>

# Glasir

*A glass interface over your branching history.*

<table>
  <tr>
    <td><img src="assets/screenshot-dark.webp" alt="Glasir in the dark Midnight theme: a commit graph with merged feature branches, tags, a remote and staged changes"></td>
    <td><img src="assets/screenshot-light.webp" alt="The same view in the light Daylight theme"></td>
  </tr>
</table>

Glasir is a fast, free and open-source Git client with a modern commit graph,
smart time-saving operations and optional AI helpers (Claude or ChatGPT). Written in
Rust (egui) for macOS (Apple Silicon), Linux and Windows. No account, no paid
tier, no telemetry.

It drives the real `git` CLI, so everything you configured for the terminal
keeps working: credential helpers (Git Credential Manager, macOS Keychain,
libsecret), SSH keys and agents, GitHub Enterprise SSO, hooks, LFS, commit
signing, partial clones and `fsmonitor`.

## Why "Glasir"?

In Norse myth, **Glasir** (Old Norse *Glasir*, said roughly "GLAH-seer") is a tree
of shining glass with leaves of red gold. It stands before the doors of Valhalla,
Odin's hall, and Snorri Sturluson calls it the most beautiful tree among gods and
men. Old poems quote it as *"Glasir stands with golden leaves before Sigtýr's
halls"*, and skalds called gold itself "Glasir's foliage".

The name fits a Git client:

- **A tree.** Your repository's history is a tree of branches, and Glasir draws it
  as one: lanes that fork and merge, every commit a leaf.
- **Glass.** The interface is built from Liquid Glass: translucent panels over a
  deep night sky.
- **Golden leaves.** The commits you craft are the gold. Glasir's job is to make
  them easy to shape, review and recover.
- **Before the doors of the hall.** It stands at the entrance: the place you pass
  through on the way into your code.

## Features

### Commit graph
- Colored lanes with rounded joins, author avatars, merge dots and a
  live **WIP** row for uncommitted changes.
- **GitHub avatars**: authors' real profile pictures (graph and commit details)
  from the origin's github.com / GitHub Enterprise host, with the token git's
  credential helper already has for it (never prompts; public github.com works
  without one). Cached on disk for a week; initials otherwise. Settings → "Show
  GitHub avatars" turns it off, with no avatar requests at all.
- **Commit details**: author and committer (when different) with absolute and
  relative dates, `Co-authored-by` trailers as co-authors, copy short or full SHA,
  a "3 modified, 1 added" summary and per-file `+/-` line counts (loaded in the
  background); renames read `old → new`, and the file list is virtualised so
  10k-file commits stay smooth.
- Branch, remote and tag **pills** joined to their commits (`+N` when several),
  with their own right-click menus; worktree branches get a tree icon.
- Loads the latest 1,000 commits for a fast start; **Load 1,000 more** at the
  bottom pages in older history. Smooth even with 85k+ commits loaded (only
  visible rows are drawn); resizable columns; the message column stays readable.
- **Hover a branch** to highlight its history and fade the rest.
- **Hide / Solo** branches, **first-parent** view.
- **Stashes** drawn next to the commit they were made on.
- **Minimap rail** with markers for HEAD, selection, WIP, search hits and branch tips.
- **⌘F search across all history**, not just the loaded commits: free text matches message, author or SHA;
  operators `author:` `committer:` `message:` `path:` `code:` (added/removed) `regex:` (changed lines)
  `after:` `before:` `merges` `no-merges`. Hits stream in ("3 of 17"), non-matches fade, ↩ / F3 step
  (loading older history as needed), a funnel filters the graph to the results; match case and recent searches.
- **Multi-select** with ⌘-click / ⇧-click: cherry-pick, squash, revert or drop
  several commits, compare oldest ↔ newest, copy SHAs.

### Staging, diffs and commits
- Stage, unstage and discard by **file, hunk or individual line**: a **Stage / Unstage
  button** appears on each file row on hover, ⌘/⇧-click selects several files (S / U or
  the buttons act on all; a multi-file discard is one backup), drag across diff lines to
  select them, and untracked files can be staged in part too (via intent-to-add; whole
  files only when `git add` would convert them: CRLF line endings, LFS or other filters;
  the diff says so).
- Unified or split diffs with **syntax highlighting** and **word-level** change marks.
- Diff controls: **ignore whitespace** (`-w`; line staging pauses while it is on),
  **context** of 3 / 10 lines or the whole file, **previous / next change**
  (↑ ↓ or F7 / ⇧F7) and a clickable change minimap.
- **Wrap** long lines (remembered) in the diff, blame and merge editor.
- Mode-only changes (`Mode changed 100644 → 100755`), binary changes with old → new
  sizes and the `\ No newline at end of file` marker are shown; diffs over ~5,000 lines
  or 1 MB wait behind **Load anyway**.
- **Image diffs** (PNG, JPEG, GIF, WebP, BMP): before / after side by side with
  dimensions and size, or a **swipe** slider; added / deleted images show one preview.
- Commit and amend, with a 72-character guide; ⌘↩ to commit, ⇧⌘↩ to stage everything
  and commit, ⇧⌘M to focus the summary. The commit box also has **Skip hooks**
  (`--no-verify`), **Push after commit** (a rejected push offers pull / force push; ⌘Z
  of a commit that is already pushed asks first, since dropping it from the remote
  needs a force push), a **co-authors** picker (people from the log become
  `Co-authored-by` trailers), `commit.template` prefill (its comment lines are stripped),
  and a detached-HEAD warning with **Create branch here**.
- **Blame** with an age heatmap at any revision (from a commit's file menu or diff),
  with **Blame at parent revision** on any line (follows renames) and back /
  forward (⌘[ / ⌘]) through the views you drill into.
- **File history** that follows renames, **folder history** (from a file menu or the
  path above a history), and **line-range history**: pick lines in a diff (or a
  blame block) and choose *History of these lines* (`git log -L`) to see each
  commit's change to just those lines.
- Compare a branch with the current one, or any commit with HEAD; **Compare working tree
  with…** any branch, tag or commit (uncommitted changes included).

### Undo everything
- ⌘Z / ⇧⌘Z undo and redo commits, amends, checkouts, merges, rebases, resets,
  branch / tag changes, cherry-picks, and **discarded work**: it is backed up as git
  objects first (exact bytes, exec bits, symlinks, staged versions, any size; the
  discard is refused if that fails). So are ignored files a checkout, pull, merge,
  rebase or Continue would overwrite, and a merge's hand resolutions before Abort.
  Undo never refuses over your edits: files you
  changed since stay as they are, a backup that can't go back waits in the stash list
  as "Glasir: …", and a failed undo offers Retry or Skip.

### Branches, merges and rebases
- **Drag a branch onto another** to merge, rebase or push.
- **Drag a commit** onto another unpushed commit of the current branch to move it
  there, squash or fixup into it (hold ⌥ to squash right away), or onto the current
  branch to cherry-pick it; one ⌘Z each, pushed commits and merges are refused.
- Sidebar keeps the **default branch** (and main / master / develop / trunk)
  pinned on top, above a separator.
- Checkout, create, rename, delete (undoable), set / unset upstream, fast-forward
  or pull a branch **without checking it out**.
- Merge, rebase, cherry-pick, revert and soft / mixed / keep / hard reset from menus.
  **Reset the current branch to any commit, branch or tag**; a hard reset lists
  the files it discards and backs them up, so ⌘Z brings back refs *and* files.
  Reset is disabled (with the reason) while a merge / rebase / cherry-pick runs.
- **Merge options** (menus, drag-and-drop, ⌘P): squash (result staged, message
  prefilled from `SQUASH_MSG`), no fast-forward, fast-forward only. **Cherry-pick
  without committing**, an optional `-x` "(cherry picked from …)" line (Settings),
  a warning before picking a commit whose change is already in HEAD, and reverting
  a merge commit says it goes against parent 1. All undoable; the staged variants
  need a clean tree and refuse to undo over later edits.
- **Interactive rebase editor**: pick, reword, squash, fixup, drop and reorder,
  with single-key shortcuts.
- **One-click history edits**: reword, squash into parent, drop, or
  **amend staged changes into any older commit**, or click a commit's message
  in its details to **edit it inline** (⌘↩ saves; warns before rewriting pushed commits).
- **Absorb staged changes**: each staged hunk becomes a fixup of the unpushed
  commit that last changed its lines and is folded in; a preview lists hunk →
  commit first, ambiguous hunks stay staged, one undo step.
- **Stacked branches**: branches built on one another show indented under their
  parent in the sidebar. Rebases and history edits move the branches below along
  (`--update-refs`, never main, `release/*`, a protected branch or a backup at
  HEAD), and a commit, amend or rewrite restacks the ones on top. **Restack**
  brings stale branches onto their moved parent (all or nothing), **Push stack**
  pushes every branch (force-with-lease + `--force-if-includes` where rewritten,
  never over a teammate's commits),
  and a stacked branch's pull request targets its parent. One undo step each.
- **Interactive cherry-pick**: pick, reword, squash, drop and reorder selected
  commits from other branches, then apply them onto HEAD in one go.
- **Split a commit**: uncommit it (mid-branch too), stage and commit the parts with
  the usual hunk / line staging, then Continue; one undo step.
- **Conflict prediction**: a toolbar warning lists files that would conflict with
  your target branch *before* you merge or pull.
- **3-way merge editor**: Current / Incoming / editable Output, per-conflict
  take-current / incoming / both, auto-resolve trivial conflicts, conflict navigation.
- **Conflict list**: each file shows its type (both modified, deleted by them, added by us, ...);
  take-side actions are named after the real branch / commit; **Mark all resolved** stages only
  text-merged files with no conflict markers left (never `-merge` / `binary` ones) and lists the rest; Continue stays disabled with a reason
  while unmerged files remain.
- **External tools**: open a conflicted file in your merge tool (backed up first, ⌘Z) and any
  file diff in your diff tool (`*.guitool` first, falling back like git does). Tools run beside
  everything else: while a merge tool is open only actions on that file (or the whole tree: checkout,
  reset, rebase, discard all) wait, with a note; the conflict banner lists open tools with Cancel, ⌘Z
  restores the file only if the tool changed it, and tool errors show; greyed out with a hint when no
  tool is configured or it needs a terminal.
- **Branch cleanup**: against a target you pick (default `origin/<default>`), lists branches merged, squash- or rebase-merged (patch-id / merge-tree equivalence; rebase-merged ones are pre-selected only when their content matches), with their upstream deleted, or stale (no commits in N days, default 90), each with its last-commit date and ahead/behind. Unpushed work is never pre-selected; main, master, develop, `release/*` and your Settings → Protected branches are never offered. Optionally deletes the same-named remote branches (confirmed; not undoable on the remote) and prunes stale remote-tracking refs (following the fetch refspecs, like `git remote prune`); local deletes and prunes undo with ⌘Z.
- Merge / rebase / cherry-pick / revert banner with Continue, Skip and Abort (undoable
  with ⌘Z), shown even when the resolved tree is clean.

### Remotes and network
- Fetch, pull (merge, fast-forward only or rebase, with autostash) and push, with
  **live progress** and **Cancel**.
- **Background auto-fetch** that never pops up a prompt; a toolbar chip when sign-in
  is needed.
- **Stash, switch and reapply** when a checkout is blocked by local changes;
  rebase-or-merge choice when a pull has diverged.
- **Safe force push** with an explicit lease on the commit you saw.
- Clone (HTTPS or SSH) and init.
- **Manage remotes** from the sidebar: add (name, URL, optional push URL, fetched
  right away), rename, edit URLs, fetch or prune one remote, remove (⌘Z restores
  its config, branch upstreams and push remotes, and remote-tracking branches).
- Push goes to the branch's upstream even under another name; a branch started
  from `origin/main` gets its own remote branch instead.

### GitHub and GitHub Enterprise (no API, no token)
- Open the repository, branch or commit page; copy commit links.
- **Create pull request** opens the compare page (per remote, URL-encoded).
- Credential prompts through Glasir's own askpass window; `insteadOf` URL
  rewrites honored.
- **Check out a pull request by number** (⌘P or a remote's menu): fetches
  `pull/<n>/head` into `pr/<n>` (again to update it); optionally show every PR as
  `<remote>/pr/<n>`.
- Commit messages link **#123** and **GH-123** to the pull request or issue.
- **Permalinks**: copy a link to a file at a commit, or open it on the remote. Also for a commit, a tag's
  release page, a file's blame and history, a compare of two selected commits or of a branch with
  the current one, and the lines you click in a commit's diff (`#L10-L20`, `?plain=1` for Markdown).
  With several remotes you pick one. Unpushed commits warn that the link would 404; only http(s)
  pages on a remote's host are ever opened.
- **Commit identity guard**: the commit box shows who you commit as; rules in
  Settings (`ghe.corp.example -> @corp.example`) warn when a repo would get the
  wrong email, with one-click **Set for this repo**.
- **Connection doctor** (⌘P "Check connection to …"): runs a non-interactive
  `ls-remote` and explains failures (sign-in, SAML SSO, SSH key or host key,
  DNS / VPN, proxy, TLS CA) with concrete fixes, plus the credential helper and
  proxy settings git uses.
- **Commit and tag signing** (GPG, SSH or X.509, as configured in git): commit
  details show a signature badge (good, unknown signer, expired, revoked, bad,
  unverifiable, unsigned) with signer and key on hover; **Sign commit** in the
  commit box and **Sign tag** for annotated tags follow `commit.gpgSign` /
  `tag.gpgSign`; **Verify signature** on a tag; signing failures explain why and
  offer to retry unsigned.

### Worktrees, stashes and tags
- **Worktrees**: list with change counts and lock state, create (new or existing
  branch), open in a new tab, lock / unlock, remove (with force / with its
  branch), prune.
- **Stashes**: stash all (with your own message, or git's default), staged only,
  a single file (right-click it in the WIP lists), or with an AI-written message;
  ⌘Z pops a fresh stash back. Apply and pop bring staged changes back staged
  (or say why they came back unstaged); ⌘Z after a pop puts the stash back and
  its files as they were. Rename a stash, create a branch from it (at the
  commit it was made on, stash popped; one ⌘Z undoes it all), delete
  (⌘/⇧-click several to delete them at once; ⌘Z brings them back in place);
  apply a single file from a stash.
- **Tags**: create (lightweight or annotated), push one or all, delete locally, on
  the remote or both at once, **move a tag** to another commit (undoable, keeps its
  annotation); annotated tag messages show on hover.

### Submodules and Git LFS
- **Submodules** sidebar section: not initialized, checked out at a different
  commit than recorded, modified or untracked content at a glance (recorded vs
  checked-out commit on hover); initialize & update, update (recursive), update
  all, sync URLs, add, remove, open in a new tab, show in file manager.
- A submodule pointer change reads **"Submodule moved from abc to def"** with the
  subjects of the commits it brings in (or drops), instead of a raw diff.
- **Git LFS**: an `LFS` badge on tracked files; pointer-file diffs show the
  object's size and id; **Track \*.ext with LFS** from a file's menu (stages
  `.gitattributes`); pull, fetch, prune and install hooks from the toolbar's `…`
  menu; a clear message when git-lfs is not installed.

### History tools
- **Reflog browser** with one-click recovery: see everywhere HEAD has been, with
  commits no branch reaches flagged; create a branch there, reset (soft / mixed /
  keep / hard, undoable) or check it out.
- **Restore a file from history**: as it is in a commit, or as it was before it
  (undoable like a discard).
- **Revert or cherry-pick a hunk or lines from any commit**: in a commit's diff,
  undo or apply just that part in your working tree (backed up, ⌘Z restores, and
  removes a file it created; a renamed file's hunk edits only the new path);
  when the code around it has moved on, a 3-way merge, leaving conflict markers
  where it can't decide.
- **Move unpushed commits to a new branch** ("oops, I committed on main"): one
  undo step, local changes untouched.
- **Visual bisect**: start from a commit's menu ("this commit is bad / good"), two
  selected commits or ⌘P; the banner shows commits and steps left with **Good /
  Bad / Skip** and **Reset**, rows get good / bad / testing marks, the next
  candidate is selected for you and the first bad commit is named at the end.
- **Patches**: copy or save commits as a patch (`git format-patch`); apply a patch
  file as commits (`git am`, with Continue / Skip / Abort) or as working-tree changes.
- **Repository maintenance**: **Perform repo maintenance** (toolbar `…` menu and ⌘P)
  packs loose objects and refreshes the commit-graph (`git maintenance`, `gc --auto`
  on old git) with progress and Cancel, never during a merge, rebase or other
  running operation. A one-time, dismissable **Optimize repository** hint appears
  when a repo has thousands of loose objects or no commit-graph. **Enable background
  maintenance** schedules git's own hourly / daily / weekly job for the repo (⌘Z undoes).

### AI (bring your own Claude or ChatGPT token)
- ✨ **Commit messages** from your staged changes (keeps your draft if it fails);
  when amending, the message covers the last commit's changes plus the staged ones.
- ✨ **Commit Composer**: the AI splits all your uncommitted changes (hunk by hunk,
  new files whole) into a series of focused commits; review the plan, edit messages,
  move changes between commits, split / merge / reorder, then create them in one
  undoable step (⌘Z).
- ✨ **Recompose commits**: "Recompose from here" on an unpushed commit (or on a
  multi-selection ending at HEAD) re-plans those commits' changes, optionally with
  your uncommitted ones, as new focused commits in the Composer; one ⌘Z brings the
  originals back. Refused for pushed commits, merges, or an operation in progress.
- ✨ **Explain** a commit, a branch, any comparison, or your working changes.
- ✨ **Pull request** title and description, opened prefilled on GitHub / GHE.
- ✨ **Stash messages** and ✨ **conflict resolution** suggestions (into the merge editor).
- Your token stays in the OS keychain and is only sent to the API URL you set.
- **Custom instructions** (Settings → AI, with Conventional Commits / Gitmoji / short-subject
  presets) are added to every AI prompt; per repo use `git config glasir.aiInstructions`,
  or the repo's `.glasir/ai.md` once you opt in with `git config glasir.aiFile true`.
- **Settings → Enable AI features** off hides every AI button and menu item.

### Workflow and comfort
- **⌘P command palette** with fuzzy search over commands, branches, tags and repos.
- **Repo tabs** show the branch, a dirty dot and ahead/behind arrows; drag to reorder, right-click to rename, close others or reveal; ⌘1–⌘9 switch. Recent repositories, live refresh when files change on disk.
- **Command log** (⇧⌘L) showing every git command Glasir ran, copyable as shell commands.
- **Embedded terminal** (⌃`, the toolbar's terminal button or ⌘P): your login shell
  in the repo folder below the graph, with several shell tabs, 256 / true colours,
  scrollback, mouse selection, copy / paste, resizing, Alt as Meta (Linux / Windows)
  and input methods. Shells keep running while the panel is hidden; closing a shell
  or quitting with a running command asks first, and closing the tab waits for it.
  Off in **Settings → Built-in terminal** (idle shells end; a busy one stays shown until done).
- **Keyboard shortcut cheat sheet**: press **?** (or ⌘/) anywhere.
- **Undo right from the toast**: after a commit, checkout, rebase, discard and
  anything else undoable, the confirmation has an **Undo** button.
- **Open in editor**, **open a terminal here** and **show in file manager** from the
  toolbar and ⌘P; files get **Open in editor** in their menus and the diff header.
  The editor defaults to `$VISUAL` / `$EDITOR` (GUI editors) or VS Code, Cursor,
  Zed or Sublime Text; the terminal to Terminal / iTerm, Windows Terminal /
  PowerShell or `x-terminal-emulator`. Both are configurable in **Settings → Tools**.
- **Custom commands** (Settings → Tools): git arguments or a shell line with
  `{repo}` `{branch}` `{sha}` `{path}` placeholders, run in the repo off the UI
  thread; commands with `{path}` show in file menus, with `{sha}` in commit menus,
  the rest in ⌘P. Output goes to a toast and the command log; ref changes undo with ⌘Z.
- **Drag a folder** (or a file in it) onto the window to open its repository.
- **Zoom** with ⌘+ / ⌘- / ⌘0, remembered between sessions.
- Friendly **empty states**: a "Make your first commit" guide in a new repository,
  how to publish when there is no remote yet (Push opens **Add remote**), a calm
  "Nothing to commit" when the working tree is clean, and dismissable first-run
  tips on the welcome page.
- Works when launched from Finder / Dock (imports your login-shell environment
  so Homebrew git, LFS, ssh-agent and signing work); warns if git is too old.

### Design
- **Liquid Glass** look: translucent glass cards, capsule controls, unified title bar; the glass edges separate areas, so there are no divider lines.
- **Six flavours**: Midnight, Graphite, Abyss (dark) and Daylight, Paper, Blossom (light).
- **Glass transparency** slider; optional native macOS glass backdrop (experimental).

## Build

Requires Rust (stable) and git ≥ 2.38.

```sh
cargo build --release
./target/release/Glasir [path/to/repo]
```

On macOS, `scripts/macos-app.sh` builds a proper **Glasir.app** (icon, name,
version, ad-hoc signature) in `target/release/`; `scripts/macos-app.sh --install`
also puts it in `/Applications`.

## AI setup

Open **Settings → AI** and pick a provider:

- **Claude (Anthropic)**: paste an Anthropic API key (`sk-ant-api…`) or an OAuth
  token (`sk-ant-oat…`, e.g. from `claude setup-token`), or set
  `ANTHROPIC_API_KEY` / `ANTHROPIC_AUTH_TOKEN`. Default model `claude-opus-5-5`.
- **ChatGPT (OpenAI)**: paste an OpenAI API key, or set `OPENAI_API_KEY`.
  Default model `gpt-5`; the base URL (default `https://api.openai.com/v1`) can
  point at any OpenAI-compatible Chat Completions endpoint.

Each provider keeps its own token, model and API base URL; every AI feature uses
the selected one. Nothing is sent anywhere until you click an AI action.

A token is read from the keychain once per run, on first use; opening
Settings never reads it. On macOS it is stored through Apple's `security` tool,
so updating Glasir doesn't make the keychain ask for your password again (a
token saved by an older version may ask once: choose **Always Allow**).

## Keyboard shortcuts

| Shortcut | Action |
| --- | --- |
| ⌘P / ⌘K | Command palette |
| ? / ⌘/ | Keyboard shortcuts |
| ⌘F | Search commits |
| F3 / ⇧F3 | Next / previous search match |
| ⌘Z / ⇧⌘Z | Undo / redo |
| ⌘↩ / ⇧⌘↩ | Commit / stage all and commit |
| ⇧⌘M | Focus the commit summary |
| ⇧⌘S / ⇧⌘U | Stage all / unstage all |
| S / U | Stage / unstage the selected files |
| ⌘L | Fetch |
| ⇧⌘L | Command log |
| ⌃` | Terminal panel |
| ↑ ↓ / J K | Move through commits |
| ⌘-click / ⇧-click | Select several commits, or WIP files to stage / unstage / discard |
| ⌘O / ⌘T / ⌘W | Open repo / new tab / close tab |
| ⌘1 – ⌘9 | Go to tab 1–8 / the last tab |
| ⌃Tab | Next tab |
| ⌘, | Settings |
| F5 / ⌘R | Refresh |
| ⌘+ / ⌘- / ⌘0 | Zoom in / out / reset |
| Esc | Close diff / editor |
| F7 / ⇧F7 | Next / previous change in a diff |
| ⌘[ / ⌘] | Back / forward in blame and history |

Interactive rebase editor: **P** pick · **R** reword · **S** squash · **F** fixup ·
**D** drop · **⌥↑ / ⌥↓** move.

(Ctrl instead of ⌘ on Linux and Windows.)

## License

MIT
