# Missing Features

Comparison against [pymobiledevice3](https://github.com/doronz88/pymobiledevice3) and [go-ios](https://github.com/danielpaulus/go-ios).

## Pairing & Device Management

- [ ] Pair device (`lockdown pair` / `lockdown unpair`)
- [ ] Save / read pair record to disk
- [ ] Enable developer mode (`amfi enable-developer-mode`, query status)
- [ ] Reboot device
- [ ] Erase / factory reset
- [ ] Device name — get / set
- [ ] Wi-Fi connections — enable / disable
- [ ] MobileGestalt key queries (`mobilegestalt <key>`)
- [ ] Device preparation / first-run setup (supervised enrolment)
- [ ] Device activation / deactivation

## File System (AFC)

- [ ] AFC shell — interactive file browser on the media partition
- [ ] Pull / push files to/from media partition
- [ ] Remove / mkdir / tree on device file system
- [ ] Per-app container file access (`apps afc`)

## Syslog & Logging

- [ ] Stream live syslog (`syslog live`)
- [ ] Stream oslog via DVT instruments protocol
- [ ] Collect / dump syslog to file

## Screenshot & Screen Recording

- [ ] Single screenshot capture (PNG output)
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

- [ ] List crash reports on device
- [ ] Download / copy crash reports
- [ ] Delete crash reports
- [ ] Parse crash report (symbolicate)
- [ ] Watch for new crash reports

## Diagnostics

- [ ] Battery info (charge level, health, cycle count)
- [ ] Battery monitoring (live)
- [ ] Disk space info
- [ ] General diagnostics dump
- [ ] Sysdiagnose capture

## Network

- [ ] Packet capture (PCAP / Wireshark live feed)
- [ ] HTTP proxy — install / remove proxy configuration profile
- [ ] Retrieve device IP address

## Notifications

- [ ] Post Darwin notification by name
- [ ] Observe Darwin notification(s)
- [ ] Observe all notifications

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
