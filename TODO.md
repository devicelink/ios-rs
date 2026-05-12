# Missing Features

Comparison against [pymobiledevice3](https://github.com/doronz88/pymobiledevice3) and [go-ios](https://github.com/danielpaulus/go-ios).

## Pairing & Device Management

- [ ] Pair device (`lockdown pair` / `lockdown unpair`)
- [ ] Save / read pair record to disk
- [ ] Enable developer mode (`amfi enable-developer-mode`, query status)
- [x] Reboot device (`ios reboot`)
- [x] Shut down device (`ios shutdown`)
- [ ] Erase / factory reset
- [x] Device name — get / set (`ios devicename [new-name]`)
- [ ] Wi-Fi connections — enable / disable
- [ ] MobileGestalt key queries (`mobilegestalt <key>`)
- [ ] Device preparation / first-run setup (supervised enrolment)
- [ ] Device activation / deactivation

## File System (AFC)

- [x] `afc ls [--long] [path]` — list directory
- [x] `afc stat <path>` — file metadata (type, size, mtime)
- [x] `afc info` — device info (model, total/free space)
- [x] `afc pull <remote> <local>` — download file or directory tree
- [x] `afc push <local> <remote>` — upload file or directory tree
- [x] `afc rm <path>` — delete file or directory
- [x] `afc mkdir <path>` — create directory
- [x] `afc mv <from> <to>` — rename / move
- [ ] AFC shell — interactive readline-based file browser
- [x] Per-app container file access (`afc --app <bundle-id>`) — implemented via house_arrest shim (RSD) / lockdownd; requires developer mode on iOS 17+

## Syslog & Logging

- [x] Stream live syslog (`ios syslog` — auto-routes to RSD shim on iOS 17.4+)
- [ ] Stream oslog via DVT instruments protocol
- [x] Collect / dump syslog to file (`ios syslog --output <file>`)

## Screenshot & Screen Recording

- [ ] Single screenshot capture — `com.apple.mobile.screenshotr` (lockdownd) works on iOS < 17;
      iOS 17.4+ requires `com.apple.corecaptured.remoteservice` (remote-XPC, not yet implemented)
- [ ] Streaming screenshot mode

## Location Spoofing

- [ ] Set simulated GPS location (lat / lon)
- [ ] Clear simulated location (restore real GPS)
- [ ] Play GPX route file for animated movement

## Process Management

- [ ] List running processes (`ps`)
- [ ] Launch installed app by bundle ID (with args / env vars)
- [ ] Kill app / process by bundle ID or PID
- [ ] Waive memory limit for a process (`memlimitoff`)

## Crash Reports

- [x] List crash reports (`ios crash ls [--long]`)
- [x] Download crash report (`ios crash pull <name> [local]`)
- [x] Delete crash report (`ios crash rm <name>`)
- [ ] Parse crash report (symbolicate)
- [ ] Watch for new crash reports

## Diagnostics

- [x] Battery info — cycle count, design/full capacity, health (`ios diagnostics battery`)
- [ ] Battery monitoring (live)
- [ ] Disk space info
- [x] General diagnostics dump (`ios diagnostics all`)
- [ ] Sysdiagnose capture

## Network

- [x] Packet capture (`ios pcap [-o file]`) — implemented; requires developer mode on iOS 17.4+ (pcapd shim does not stream without it)
- [ ] HTTP proxy — install / remove proxy configuration profile
- [ ] Retrieve device IP address

## Notifications

- [x] Post Darwin notification (`ios notification post <name>`)
- [x] Observe Darwin notifications (`ios notification observe [<name>]`)

## Debugging

- [ ] Start LLDB debug server and attach debugger
- [ ] DTX / protocol proxy for reverse engineering
- [ ] Fetch developer symbols from device

## Device Condition Profiles

- [ ] List condition profiles (network throttling, GPU stress, …)
- [ ] Enable / clear a condition profile

## Accessibility

- [ ] AssistiveTouch — enable / disable / get
- [ ] VoiceOver — enable / disable / get
- [ ] Zoom — enable / disable / get
- [ ] Time format — switch between 12 h and 24 h
- [ ] Reset all accessibility settings
- [ ] Accessibility settings — get / set arbitrary key

## Springboard

- [ ] Query SpringBoard state keys
- [ ] Get home-screen wallpaper image
- [ ] App icon metrics / icon image

## Web Automation (WebInspector)

- [ ] List open browser tabs
- [ ] Open URL in automation session
- [ ] JavaScript REPL (with and without automation session)
- [ ] Chrome DevTools Protocol (CDP) access
- [ ] Reload / navigate pages

## Backup & Restore

- [ ] Full device backup (MobileBackup2)
- [ ] Incremental / partial backup
- [ ] Restore from backup
- [ ] Backup info / list contents
- [ ] Change / remove backup encryption password
- [ ] Erase device via backup service

## Firmware & Recovery

- [ ] Firmware update from IPSW (or URL)
- [ ] Enter / exit Recovery mode
- [ ] DFU mode workflows
- [ ] TSS / SHSH blob handling

## Profiles & MDM

- [ ] List / install / remove provisioning profiles
- [ ] List / install / remove MDM configuration profiles
- [ ] Device supervision
- [ ] Identity authentication management (idam)
