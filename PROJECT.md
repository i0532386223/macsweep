# MacSweep for macOS

This fork of nihal4/MacSweep helps a Mac user review disk usage and remove selected items. The main Rust application uses egui. A separate AppKit helper displays CPU, memory and free space in the menu bar when enabled. The shared MacSweep Xcode scheme builds both programs and packages the helper inside the application.

## Safety changes

- To quit an application during removal, MacSweep uses AppKit. It compares the device and inode of each running bundle with the selected bundle. If the app declines to quit or stays open, MacSweep skips its removal.
- System Junk enumerates each approved root through an open directory descriptor. Permanent removal in System Junk, Browser Cleanup and Empty Trash opens each parent without following links and operates relative to those descriptors. The recursive walk stops at a different filesystem. Only Trash operations can unlock a selected object. A regular file with multiple hard links stays locked.
- Finder processes ordinary Trash requests. On refusal, MacSweep reports the paths that remain. MacSweep runs with the user's privileges throughout the operation. When it moves a symbolic link itself, an exclusive rename preserves existing items in the Trash.
- Duplicate groups use the BLAKE3 digest of the entire file. Before a move, MacSweep reads the selected files and a kept copy again, checks them against the saved digest, and checks current file identity and timestamps. A change or a read error skips the affected files. Another application can still modify a file after the last check. Duplicate removal goes to the Trash.
- Browser favicon queries use a read-only connection to the copy of SQLite pinned by Cargo.lock. The main app checks this fork's GitHub Releases page for updates. The user opens the release page to install an update.

The main app logs startup, checks before removal and failures to the `me.i0532386223.macsweep` subsystem, in the `safety` category. Logs use counts and system error codes. The interface shows the affected paths to the user when an item stays in place.

## Build and verify

Install Xcode and a Rust compiler compatible with the versions in Cargo.lock. Open `MacSweep.xcodeproj` and build the **MacSweep** scheme. The application is written to `dist/MacSweep.app`.

```sh
cargo fetch --locked
cargo fmt --all --check
cargo clippy --workspace --all-targets --locked --offline -- -D warnings -A clippy::all
cargo test --workspace --locked --offline
xcodebuild -project MacSweep.xcodeproj -scheme MacSweep -configuration Release build
```

The Xcode scheme selects a valid **Developer ID Application** identity for the configured development team. To select a specific certificate, pass its SHA-1 identifier as the build setting `MACSWEEP_SIGN_IDENTITY`. Use `ADHOC=1` for a local test build without a certificate. The packaging script signs the helper first and the main app second, with the hardened runtime and an Apple timestamp for Developer ID builds. Only the main app requests the Apple Events entitlement so it can ask Finder to move files. Signing and notarization are separate operations.

```sh
codesign --verify --strict --deep --verbose=2 dist/MacSweep.app
codesign -d --verbose=4 dist/MacSweep.app
```

## Verification on November 24, 2026

- Rust 1.97.1 and Xcode 27.1 built the arm64 Release application. The executable declares macOS 11.0 as its minimum. Runtime checks used the current Apple Silicon Mac.
- All 68 main application tests and all 5 monitor tests passed. The formatting and Clippy checks passed. Filesystem tests used temporary directories, including a rejected Finder request, an existing item in the Trash, linked directories and an edited duplicate with the original size.
- A separate AppKit check launched a temporary application. Selecting a different bundle with the same name and bundle ID kept the running instance open. Selecting its own bundle let it quit normally.
- OSV checked all 311 registry package versions in Cargo.lock. It returned no advisory for any of the 160 packages in the macOS application and monitor build graph. The lockfile also contains `chrono` for another target. Its remaining advisory reports that maintenance has stopped. This build includes SQLite 3.54.1. [RustSec SQLite advisory](https://rustsec.org/advisories/RUSTSEC-2026-0459.html) · [RustSec chrono notice](https://rustsec.org/advisories/RUSTSEC-2026-0543.html)
- Both bundles passed strict signature verification. Each has the Developer ID Application signature for team `7P3YR8EC6R`, the hardened runtime and an Apple timestamp. Gatekeeper reported `Unnotarized Developer ID`. No notarization request was submitted.
- The signed main app opened a window, quit normally and returned a read-only health report. File removal tests used fixtures. Testing did not clean the user's files or grant Full Disk Access.

## Comparison with Purge

The comparison uses MacSweep upstream commit `b7480f4` and Purge commit `db58c41`. It is based on the two source trees. The changes above cover the safety of existing MacSweep features.

| Area | MacSweep | Additional Purge functions |
|---|---|---|
| Disk use and cache | Disk and Trash totals, user caches, logs, Smart Scan | A disk map, a breakdown of other space, selected container and system caches, safety labels and exclusions |
| Developer tools | Xcode DerivedData, DeviceSupport, simulator caches, npm and Gradle caches | Individual simulators and runtimes, Xcode archives and Metal Toolchain, more package caches, stale project artifacts, checks for active projects |
| Large files | Size and age filters, categories, Finder, Quick Look, content-based duplicates | Extra scan folders and attached disks, VM bundles as a unit, incomplete downloads, evicting the local copy of an uploaded iCloud file |
| AI storage | Hugging Face and PyTorch cache folders, large model files and Chromium model downloads | Inventory by model and store, including Ollama and LM Studio, accounting for shared blobs, and separate Apple Intelligence asset management on supported Macs |
| App removal | Bundles and selected leftovers of installed apps, orphaned launch agents | Review of leftovers after an app has gone, optional watcher, Finder and Shortcuts integration, removal of command-line tools through their package manager |
| Cleanup | Manual confirmation, permanent removal of junk and browser caches, Trash for user files | Trash by default for ordinary cleanup, history, optional scheduled cleaning and a menu-bar cleaning action |
| Other capabilities | Battery and SSD health, per-site Chromium storage cleanup, CPU and RAM monitor | Management of Time Machine local snapshots, localized UI, signed Sparkle updates |

Purge also has an optional privileged helper. Its XPC listener sets a code-signing requirement for the client, and its service checks the caller and the requested paths. Most files go to the Trash. Some approved actions, including simulator and snapshot removal, are permanent. Its Apple Intelligence feature uses a private Apple framework and requires a user-approved profile for its removal workflow. These are findings from source inspection. Purge was not installed or run for this comparison. [Purge feature details](https://github.com/jithin-sabu/purge-app/blob/db58c41338dfe95b7f97de16741bafee809bfcf0/README.md) · [Deletion policy](https://github.com/jithin-sabu/purge-app/blob/db58c41338dfe95b7f97de16741bafee809bfcf0/purge/Services/DeletionSafetyPolicy.swift) · [Helper](https://github.com/jithin-sabu/purge-app/tree/db58c41338dfe95b7f97de16741bafee809bfcf0/PurgeHelper) · [Apple asset worker](https://github.com/jithin-sabu/purge-app/blob/db58c41338dfe95b7f97de16741bafee809bfcf0/PurgeAssetWorker/UnifiedAssetCalls.swift)

Purge pins Sparkle 2.9.4. Two high-severity Sparkle advisories include that version. Sparkle fixed both specific issues in 2.9.6. This finding applies to the reviewed dependency lock. The published Purge binary was outside this review. MacSweep has no Sparkle dependency. [Purge dependency lock](https://github.com/jithin-sabu/purge-app/blob/db58c41338dfe95b7f97de16741bafee809bfcf0/purge.xcodeproj/project.xcworkspace/xcshareddata/swiftpm/Package.resolved) · [Update path advisory](https://github.com/sparkle-project/Sparkle/security/advisories/GHSA-3x7w-j75x-ppq5) · [Privileged cleanup advisory](https://github.com/sparkle-project/Sparkle/security/advisories/GHSA-4v99-qgq9-6pxp)
