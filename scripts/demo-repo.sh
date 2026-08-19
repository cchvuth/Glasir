#!/bin/bash
# Builds a demo repository with an interesting graph for the README screenshots:
#   scripts/demo-repo.sh /tmp/aurora
#   GLASIR_SCREENSHOT=dark.bmp ./target/debug/Glasir /tmp/aurora
#   GLASIR_SCREENSHOT_VIEW=flavor:Daylight GLASIR_SCREENSHOT=light.bmp ./target/debug/Glasir /tmp/aurora
set -e
D=${1:?target dir}
rm -rf "$D" "$D.remote"
git init -q -b main "$D"
cd "$D"
git config commit.gpgsign false
now=$(date +%s)
n=0
# who "Name" mail
who() { export GIT_AUTHOR_NAME="$1" GIT_AUTHOR_EMAIL="$2" GIT_COMMITTER_NAME="$1" GIT_COMMITTER_EMAIL="$2"; }
# c file "message" [hours ago]
c() {
  n=$((n + 1))
  local t=$((now - ${3:-$((200 - n * 3))} * 3600))
  export GIT_AUTHOR_DATE="$t" GIT_COMMITTER_DATE="$t"
  mkdir -p "$(dirname "$1")"
  echo "$2 ($n)" >>"$1"
  git add "$1"
  git commit -q -m "$2"
}
m() { # m branch "message"
  n=$((n + 1))
  local t=$((now - (200 - n * 3) * 3600))
  export GIT_AUTHOR_DATE="$t" GIT_COMMITTER_DATE="$t"
  git merge -q --no-ff "$1" -m "$2"
}
ADA="Ada Park"; BEN="Ben Okafor"; CHLOE="Chloé Martin"; DEV="Dev Raman"; EMI="Emi Sato"

who "$ADA" ada@aurora.dev
c README.md "Initial commit: Aurora weather dashboard"
c src/app.ts "Scaffold the app shell and router"
c src/api/client.ts "Add the forecast API client"
git tag -a v0.1.0 -m "First preview"

git checkout -q -b feature/radar
who "$BEN" ben@aurora.dev
c src/radar/map.ts "Render the radar map with tiles"
git checkout -q main
who "$CHLOE" chloe@aurora.dev
c src/ui/theme.css "Add dark and light themes"
git checkout -q feature/radar
who "$BEN" ben@aurora.dev
c src/radar/loop.ts "Animate the last two hours of radar"
git checkout -q -b feature/alerts main
who "$DEV" dev@aurora.dev
c src/alerts/feed.ts "Poll severe weather alerts"
git checkout -q main
who "$ADA" ada@aurora.dev
c src/api/cache.ts "Cache forecasts for ten minutes"
git checkout -q feature/alerts
who "$DEV" dev@aurora.dev
c src/alerts/banner.ts "Show an alert banner with severity colours"
git checkout -q main
who "$ADA" ada@aurora.dev
m feature/radar "Merge branch 'feature/radar'"
git checkout -q feature/alerts
who "$DEV" dev@aurora.dev
c src/alerts/push.ts "Send push notifications for new alerts"
git checkout -q main
who "$EMI" emi@aurora.dev
c src/ui/hourly.ts "Hourly forecast strip"
m feature/alerts "Merge branch 'feature/alerts'"
git tag -a v0.2.0 -m "Radar and alerts"

git checkout -q -b fix/timezone
who "$CHLOE" chloe@aurora.dev
c src/api/time.ts "Fix sunrise times across time zones"
git checkout -q main
who "$BEN" ben@aurora.dev
c src/radar/map.ts "Speed up tile loading on slow networks"
m fix/timezone "Merge branch 'fix/timezone'"

git checkout -q -b feature/widgets
who "$EMI" emi@aurora.dev
c src/widgets/today.ts "Add a Today widget"
c src/widgets/week.ts "Add a week-ahead widget"
git checkout -q -b feature/widget-themes
c src/widgets/themes.css "Theme widgets to match the app"
git checkout -q main
who "$ADA" ada@aurora.dev
c src/app.ts "Lazy-load the radar route"
c package.json "Bump dependencies"
git checkout -q -b feature/air-quality
who "$DEV" dev@aurora.dev
c src/air/index.ts "Air quality index from the new API"
c src/air/chart.ts "Chart pollen and PM2.5 for the week"
git checkout -q main
who "$CHLOE" chloe@aurora.dev
c src/ui/a11y.ts "Improve screen reader labels"
git tag -a v0.3.0 -m "Accessibility pass"
who "$BEN" ben@aurora.dev
c src/radar/loop.ts "Pause the radar loop when hidden"
c src/api/client.ts "Retry forecast requests with backoff"

# A remote that is a little behind main, with the feature branches pushed.
git clone -q --bare "$D" "$D.remote"
git remote add origin "$D.remote"
git fetch -q origin
git branch -q -u origin/main main
git push -q origin feature/widgets feature/air-quality
who "$ADA" ada@aurora.dev
c src/ui/settings.ts "Units setting: metric or imperial"
c src/ui/settings.ts "Remember the last viewed city"

# A stash and some uncommitted work for the WIP row.
echo "// TODO: offline mode" >>src/app.ts
git stash push -q -m "Offline mode experiment"
echo "export const REFRESH_MINUTES = 5;" >>src/api/cache.ts
echo "Hourly strip scrolls to now on open." >>README.md
git add README.md
echo "export {};" >src/ui/radar-legend.ts

# The commit box shows who you commit as.
git config user.name "Ada Park"
git config user.email ada@aurora.dev
