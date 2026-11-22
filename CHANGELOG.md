# Changelog

## v0.3.0

### New: Mac Health
- **Battery:** Apple's own Maximum Capacity and Condition (the same values System Settings shows), charge cycles against the number the battery is designed for, the current charge, and the battery's raw mAh readings.
- **Storage:** macOS's own S.M.A.R.T. verdict plus the SSD's health log: share of its rated lifespan used, spare blocks left, data written and read, power-on hours, unexpected power losses, error counts and temperature.
- **System:** model, chip, memory, macOS version, uptime, thermal state, memory in use with memory pressure, and swap.
- Nothing is estimated. If macOS doesn't report a value, MacSweep shows "Not available". Hover any value to see where it comes from.

**Validated:** on a MacBook Air M1 (macOS 27.3), all 23 values with a reference tool match Apple's tools (System Settings, `system_profiler`, `ioreg`, `sysctl`) and smartctl exactly. Two live values (temperature, data read) differed by a hair because the tools read seconds apart. Full table and method: [docs/health-validation.md](docs/health-validation.md). Run `scripts/validate-health.sh` to check your own Mac.

Apple calculates Maximum Capacity its own way. On the test Mac the raw mAh ratios were 89% and 86% while Apple showed 90%, so MacSweep never calculates it and always shows Apple's figure.

### New: menu bar monitor (optional, off by default)
- Shows CPU, memory and free disk space in the menu bar (`CPU 12%  RAM 61%  120 GB free`). Choose which, and update **live (every second)**, every 2 s or every 5 s.
- A separate tiny native helper, so MacSweep itself doesn't stay open. Settings apply instantly. Turning it off, in Mac Health or from its own menu, removes it completely.
- Its menu shows details (memory pressure, disk space) and what the monitor itself is using.

**Measured cost** on a MacBook Air M1, macOS 27.3, 5 minutes per setting (`scripts/bench-monitor.sh`):

| Update rate | CPU (of one core) | Energy Impact | Memory (Activity Monitor), avg / max |
|---|---|---|---|
| Live (1 s) | 2.21 % | 2.29 | 12.8 / 14.7 MB |
| Every 2 s | 1.33 % | 1.34 | 18.5 / 22.0 MB |
| Every 5 s | 0.46 % | 0.28 | 20.0 / 22.0 MB |

A **1-hour run at Live** showed no memory growth: 13.1 MB on average, never above 14.7 MB, at 2.34 % CPU and Energy Impact 2.25.

The M1 has 8 cores, so Live is about 0.3 % of the whole Mac. Profiling showed our own code is under 1 ms per update. The rest is macOS redrawing the menu-bar item, which every monitor that updates each second pays. We tried drawing the numbers as an image instead of text: no measurable difference, so the simpler text stays.

- The monitor stops completely while the screen is off or another user's session is in front, and restarts itself when MacSweep is updated.
- It must run from your Applications folder (not Downloads or a disk image), so it keeps working after a restart.

### Security
- Every system command is called by its full path (`/bin/launchctl`, `/usr/bin/osascript` …), so a changed `PATH` can't substitute another program.
- The "Relaunch" button after granting Full Disk Access quoted the app's path unsafely for the shell; a folder name containing `$` or a backtick could run a command. Fixed.
- The release pipeline pins every third-party GitHub Action to an exact commit, and CI runs with read-only permissions.

### Development
- `main` is protected: every change goes through a pull request, and CI (formatting, tests, app build) must pass.

## v0.2.0

### New: Duplicates
- Finds identical files in Desktop, Documents, Downloads, Pictures, Movies and Music. Files are matched by content (BLAKE3), not by name.
- Nothing is selected for you. Tick a whole group, a single copy, or use **Select all extra copies**. One copy of each file is always kept.
- **Preview** (Quick Look) and **Show in Finder** on every copy, so you can check before removing.
- Never looks inside Photos or Music libraries, app bundles, `~/Library`, hidden folders, build folders (`target`, `node_modules`, virtual environments) or iCloud-only files.

### Safer by default
- Every action asks first. Clean, Remove, Uninstall and Launch Agents show what will happen, with how many items and how much space, before anything moves.
- Clean warns when Trash items are included, because cleaning is permanent.
- Caches of apps that are open right now are marked **In use** and left unticked.
- Large & Old Files no longer lists app data, browser profiles, Mail, Photos or Music library internals, the Trash, or iCloud-only files. Deleting an iCloud-only file would remove it from every device.
- If a ticked file is hidden by the current filter, Large Files says so.
- The Uninstaller never treats `/Applications/Utilities` or your own folders as one app, and never picks up a sibling app's settings (for example Chrome Canary when you remove Chrome).
- Uninstalling warns you when you've ticked your own data, such as profiles or accounts.
- Launch Agents warns before removing a background item that belongs to an app that is still installed.

### Uninstaller
- Install folders (an app plus its helper apps) are listed and removed as one.
- Also finds the app's files in `/Library`: background services, helper tools and preferences for all users.
- Stops the app's background agents before removing them.
- Apps that macOS installed as the system owner can be removed with your password. Broken shortcuts are labelled and removed properly.
- Real icons for apps that don't ship an `.icns` file.

### Fixes
- Moving many files to the Trash plays the sound once instead of in a loop.
- Empty Trash also empties the Trash on external drives, unlocks locked files and tells you if something couldn't be deleted.
- The Trash size in the sidebar updates when you trash files in Finder.
- Free space now matches Finder, because it includes space macOS can clear on its own. The sidebar also shows your disk's real name.
- "Last opened" in Large Files comes from Spotlight, like Finder.
- A crash inside a scan shows a message instead of closing the app.

### Interface
- You can move the window by dragging its top edge, and double-click there to zoom.
- Every screen works down to the smallest window size, with more space between list and detail panels.
- **Change size** after a scan, and a notice when a scan was stopped early.
- **Done** returns to the list instead of starting a new scan.
- Small polish: pointer cursor on clickable rows, aligned headers, and wording such as "1 item".
