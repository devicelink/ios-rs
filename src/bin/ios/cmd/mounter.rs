/// Developer Disk Image (DDI) mounting for iOS 17+.
///
/// Downloads the personalized DDI from github.com/doronz88/DeveloperDiskImage,
/// obtains an IM4M ticket from Apple's TSS, uploads Image.dmg to the device,
/// and mounts it at /System/Developer — unlocking dtservicehub and other
/// developer services in the trusted RSD catalog.
///
/// Flow (mirrors pymobiledevice3 auto_mount_personalized):
///   1. Download Image.dmg + BuildManifest.plist + Image.dmg.trustcache (cached)
///   2. Connect to com.apple.mobile.mobile_image_mounter.shim.remote
///   3. LookupImage → already mounted?
///   4. QueryPersonalizationManifest(sha384(Image.dmg))
///      → hit: IM4M in hand; miss: reconnect and go to TSS flow
///   5. QueryPersonalizationIdentifiers + QueryNonce
///   6. POST http://gs.apple.com/TSS/controller?action=2 → ApImg4Ticket (IM4M)
///   7. ReceiveBytes → stream Image.dmg raw → wait Complete
///   8. MountImage with IM4M + trustcache
use std::io::{Read, Write};
use std::path::PathBuf;
use anyhow::{bail, Context, Result};
use plist::Value;
use sha2::{Digest, Sha384};

use ios_rs::tunnel::ConnectionMode;
use crate::cmd::open_session;
use crate::cmd::output::{print_json, ActionResult, OutputMode};

#[derive(serde::Serialize)]
struct MounterStatus {
    mounted: bool,
}

// ── DDI repo constants ────────────────────────────────────────────────────────

const DDI_REPO: &str = "doronz88/DeveloperDiskImage";
const DDI_IMAGE:       &str = "PersonalizedImages/Xcode_iOS_DDI_Personalized/Image.dmg";
const DDI_MANIFEST:    &str = "PersonalizedImages/Xcode_iOS_DDI_Personalized/BuildManifest.plist";
const DDI_TRUSTCACHE:  &str = "PersonalizedImages/Xcode_iOS_DDI_Personalized/Image.dmg.trustcache";

// ── public entry points ───────────────────────────────────────────────────────

pub fn mount(udid: Option<&str>, output: OutputMode) -> Result<()> {
    let mut session = open_session(udid, ConnectionMode::Rsd)?;

    // ── 1. Fetch DDI files (cached) ──────────────────────────────────────────
    eprintln!("[mounter] fetching DDI files from github.com/{DDI_REPO}…");
    let (image_bytes, manifest_bytes, trustcache_bytes) = fetch_ddi_files()
        .context("download DDI files")?;
    eprintln!("[mounter] Image.dmg: {} MB", image_bytes.len() / 1_048_576);

    // SHA-384 of Image.dmg used for QueryPersonalizationManifest
    let image_sha384: Vec<u8> = {
        let mut h = Sha384::new();
        h.update(&image_bytes);
        h.finalize().to_vec()
    };

    // ── 2. Connect to mounter shim service ───────────────────────────────────
    let socket = session.connect_rsd_shim(
            "com.apple.mobile.mobile_image_mounter.shim.remote")
        .context("connect mounter shim")?;

    // ── 3. LookupImage ───────────────────────────────────────────────────────
    let mut sock = socket;
    {
        let req = plist_dict! {
            "Command" => "LookupImage",
            "ImageType" => "Personalized"
        };
        send_plist(&mut sock, &req)?;
        let resp = recv_plist(&mut sock)?;
        if resp_image_present(&resp) {
            eprintln!("[mounter] already mounted.");
            return Ok(());
        }
    }

    // ── 4. QueryPersonalizationManifest ──────────────────────────────────────
    let im4m: Vec<u8> = {
        let req = plist_dict! {
            "Command"              => "QueryPersonalizationManifest",
            "PersonalizedImageType"=> "DeveloperDiskImage",
            "ImageType"            => "DeveloperDiskImage",
            "ImageSignature"       => Value::Data(image_sha384.clone())
        };
        send_plist(&mut sock, &req)?;

        match recv_plist(&mut sock) {
            Ok(resp) => {
                // Cache hit — device returned the IM4M
                if let Some(Value::Data(b)) = resp.as_dictionary().and_then(|d| d.get("ImageSignature")) {
                    b.clone()
                } else {
                    // iOS 26: returns Error dict on cache miss instead of closing connection
                    eprintln!("[mounter] no cached manifest, requesting TSS ticket…");
                    drop(sock);
                    sock = session.connect_rsd_shim(
                            "com.apple.mobile.mobile_image_mounter.shim.remote")
                        .context("reconnect mounter shim")?;
                    tss_flow(&mut session, &mut sock, &manifest_bytes, &image_sha384)?
                }
            }
            Err(_) => {
                // Cache miss — device closed the connection (pre-iOS 26 behaviour)
                eprintln!("[mounter] no cached manifest, requesting TSS ticket…");
                drop(sock);
                sock = session.connect_rsd_shim(
                        "com.apple.mobile.mobile_image_mounter.shim.remote")
                    .context("reconnect mounter shim")?;
                tss_flow(&mut session, &mut sock, &manifest_bytes, &image_sha384)?
            }
        }
    };

    // ── 8. ReceiveBytes + upload ─────────────────────────────────────────────
    eprintln!("[mounter] uploading Image.dmg ({} MB)…", image_bytes.len() / 1_048_576);
    {
        let req = plist_dict! {
            "Command"        => "ReceiveBytes",
            "ImageType"      => "Personalized",
            "ImageSize"      => Value::Integer((image_bytes.len() as i64).into()),
            "ImageSignature" => Value::Data(im4m.clone())
        };
        send_plist(&mut sock, &req)?;
        let ack = recv_plist(&mut sock).context("ReceiveBytes ack")?;
        let status = ack.as_dictionary()
            .and_then(|d| d.get("Status"))
            .and_then(|v| v.as_string())
            .unwrap_or("");
        if status != "ReceiveBytesAck" {
            bail!("ReceiveBytes: unexpected status {status:?}");
        }
        // Stream raw bytes
        sock.write_all(&image_bytes).context("write Image.dmg")?;
        sock.flush()?;
        let complete = recv_plist(&mut sock).context("upload complete")?;
        let status2 = complete.as_dictionary()
            .and_then(|d| d.get("Status"))
            .and_then(|v| v.as_string())
            .unwrap_or("");
        if status2 != "Complete" {
            bail!("upload: unexpected status {status2:?}");
        }
    }

    // ── 9. MountImage ────────────────────────────────────────────────────────
    eprintln!("[mounter] mounting…");
    {
        let req = plist_dict! {
            "Command"        => "MountImage",
            "ImageType"      => "Personalized",
            "ImageSignature" => Value::Data(im4m),
            "ImageTrustCache"=> Value::Data(trustcache_bytes)
        };
        send_plist(&mut sock, &req)?;
        let resp = recv_plist(&mut sock).context("MountImage")?;
        let status = resp.as_dictionary()
            .and_then(|d| d.get("Status"))
            .and_then(|v| v.as_string())
            .unwrap_or("");
        if status != "Complete" {
            bail!("MountImage: {status:?}  full: {resp:?}");
        }
    }

    if output.is_json() {
        print_json(&ActionResult::with_msg("mounted"))?;
    } else {
        eprintln!("[mounter] mounted. Developer services are now available.");
    }
    Ok(())
}

pub fn status(udid: Option<&str>, output: OutputMode) -> Result<()> {
    let mut session = open_session(udid, ConnectionMode::Rsd)?;
    let mut sock = session.connect_rsd_shim(
            "com.apple.mobile.mobile_image_mounter.shim.remote")
        .context("connect mounter shim")?;

    let mut mounted = false;
    for img_type in &["Personalized", "Developer"] {
        let req = plist_dict! { "Command" => "LookupImage", "ImageType" => *img_type };
        send_plist(&mut sock, &req)?;
        match recv_plist(&mut sock) {
            Ok(resp) => {
                let present = resp_image_present(&resp);
                if present { mounted = true; }
                if !output.is_json() {
                    eprintln!("[mounter] LookupImage({img_type}): {resp:?}");
                    if present {
                        println!("Developer disk image ({img_type}): mounted");
                    } else {
                        println!("Developer disk image ({img_type}): not mounted");
                    }
                }
            }
            Err(e) => {
                if !output.is_json() {
                    eprintln!("[mounter] LookupImage({img_type}) error: {e}");
                }
            }
        }
    }
    if output.is_json() {
        print_json(&MounterStatus { mounted })?;
    }
    Ok(())
}

// ── DDI download + cache ──────────────────────────────────────────────────────

fn cache_dir() -> PathBuf {
    let base = dirs_or_home();
    base.join("ios-rs").join("ddi")
}

fn dirs_or_home() -> PathBuf {
    // macOS: ~/Library/Caches/  Linux: ~/.cache/
    #[cfg(target_os = "macos")]
    let base = std::env::var("HOME")
        .map(|h| PathBuf::from(h).join("Library").join("Caches"))
        .unwrap_or_else(|_| PathBuf::from("/tmp"));
    #[cfg(not(target_os = "macos"))]
    let base = std::env::var("XDG_CACHE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|_| {
            std::env::var("HOME")
                .map(|h| PathBuf::from(h).join(".cache"))
                .unwrap_or_else(|_| PathBuf::from("/tmp"))
        });
    base
}

fn fetch_ddi_files() -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let dir = cache_dir();
    std::fs::create_dir_all(&dir)?;

    let img_path  = dir.join("Image.dmg");
    let mfst_path = dir.join("BuildManifest.plist");
    let tc_path   = dir.join("Image.dmg.trustcache");

    // Use already-cached files if present
    if img_path.exists() && mfst_path.exists() && tc_path.exists() {
        eprintln!("[mounter] using cached DDI files");
        return Ok((
            std::fs::read(&img_path)?,
            std::fs::read(&mfst_path)?,
            std::fs::read(&tc_path)?,
        ));
    }

    // Prefer the CoreDevice local DDI (/Library/Developer/CoreDevice/CandidateDDIs/iOS_DDI.dmg)
    // — newer build manifest than doronz88's repo, no download needed.
    let core_device_ddi = PathBuf::from(
        "/Library/Developer/CoreDevice/CandidateDDIs/iOS_DDI.dmg");
    if core_device_ddi.exists() {
        eprintln!("[mounter] extracting DDI files from CoreDevice local cache…");
        match extract_from_core_device_ddi(&core_device_ddi) {
            Ok((img, mfst, tc)) => {
                std::fs::write(&img_path,  &img)?;
                std::fs::write(&mfst_path, &mfst)?;
                std::fs::write(&tc_path,   &tc)?;
                return Ok((img, mfst, tc));
            }
            Err(e) => eprintln!("[mounter] CoreDevice DDI extract failed: {e}, falling back to GitHub"),
        }
    }

    // Fall back to downloading from doronz88/DeveloperDiskImage
    eprintln!("[mounter] downloading DDI from github.com/{DDI_REPO}…");
    let manifest_bytes = github_download(DDI_REPO, DDI_MANIFEST)
        .context("download BuildManifest.plist")?;
    std::fs::write(&mfst_path, &manifest_bytes)?;

    eprintln!("[mounter] downloading Image.dmg…");
    let image_bytes = github_download(DDI_REPO, DDI_IMAGE)
        .context("download Image.dmg")?;
    std::fs::write(&img_path, &image_bytes)?;

    let trustcache_bytes = github_download(DDI_REPO, DDI_TRUSTCACHE)
        .context("download Image.dmg.trustcache")?;
    std::fs::write(&tc_path, &trustcache_bytes)?;

    Ok((image_bytes, manifest_bytes, trustcache_bytes))
}

/// Mount the CoreDevice DDI, extract Image.dmg + BuildManifest + trustcache.
fn extract_from_core_device_ddi(ddi_path: &std::path::Path) -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
    let mount_point = "/tmp/ios-rs-ddi-extract";
    let _ = std::process::Command::new("hdiutil")
        .args(["detach", mount_point, "-quiet", "-force"])
        .output();

    let out = std::process::Command::new("hdiutil")
        .args(["attach", ddi_path.to_str().unwrap(),
               "-readonly", "-mountpoint", mount_point, "-quiet", "-nobrowse"])
        .output()
        .context("hdiutil attach")?;
    if !out.status.success() {
        bail!("hdiutil attach failed: {}", String::from_utf8_lossy(&out.stderr));
    }

    let result = (|| -> Result<(Vec<u8>, Vec<u8>, Vec<u8>)> {
        let restore = PathBuf::from(mount_point).join("Restore");
        let manifest_bytes = std::fs::read(restore.join("BuildManifest.plist"))
            .context("read BuildManifest.plist")?;
        let manifest: Value = plist::from_bytes(&manifest_bytes)
            .context("parse BuildManifest.plist")?;

        // Find the PersonalizedDMG and LoadableTrustCache file paths from the manifest
        let identity = manifest.as_dictionary()
            .and_then(|d| d.get("BuildIdentities"))
            .and_then(|v| v.as_array())
            .and_then(|a| a.first())
            .ok_or_else(|| anyhow::anyhow!("no BuildIdentities"))?;
        let mfst_entries = identity.as_dictionary()
            .and_then(|d| d.get("Manifest"))
            .and_then(|v| v.as_dictionary())
            .ok_or_else(|| anyhow::anyhow!("no Manifest in BuildIdentity"))?;

        let img_rel = mfst_entries.get("PersonalizedDMG")
            .and_then(|v| v.as_dictionary())
            .and_then(|d| d.get("Info"))
            .and_then(|v| v.as_dictionary())
            .and_then(|d| d.get("Path"))
            .and_then(|v| v.as_string())
            .ok_or_else(|| anyhow::anyhow!("no PersonalizedDMG/Info/Path"))?;
        let tc_rel = mfst_entries.get("LoadableTrustCache")
            .and_then(|v| v.as_dictionary())
            .and_then(|d| d.get("Info"))
            .and_then(|v| v.as_dictionary())
            .and_then(|d| d.get("Path"))
            .and_then(|v| v.as_string())
            .ok_or_else(|| anyhow::anyhow!("no LoadableTrustCache/Info/Path"))?;

        let image_bytes      = std::fs::read(restore.join(img_rel))
            .context("read PersonalizedDMG")?;
        let trustcache_bytes = std::fs::read(restore.join(tc_rel))
            .context("read trustcache")?;

        Ok((image_bytes, manifest_bytes, trustcache_bytes))
    })();

    let _ = std::process::Command::new("hdiutil")
        .args(["detach", mount_point, "-quiet"])
        .output();

    result
}

fn github_download(repo: &str, path: &str) -> Result<Vec<u8>> {
    // Use raw.githubusercontent.com for direct file access (no API token needed)
    let url = format!("https://raw.githubusercontent.com/{repo}/refs/heads/main/{path}");
    let resp = ureq::get(&url)
        .call()
        .with_context(|| format!("GET {url}"))?;
    let mut buf = Vec::new();
    resp.into_reader().read_to_end(&mut buf)?;
    Ok(buf)
}

// ── TSS personalization ───────────────────────────────────────────────────────

/// Steps 5–7: query identifiers + nonce, call TSS, reconnect socket.
fn tss_flow(
    session:        &mut ios_rs::tunnel::DeviceSession,
    sock:           &mut ios_rs::usbmux::MuxSocket,
    manifest_bytes: &[u8],
    _image_sha384:  &[u8],
) -> Result<Vec<u8>> {
    // 5. QueryPersonalizationIdentifiers
    let req_ids = plist_dict! {
        "Command"               => "QueryPersonalizationIdentifiers",
        "PersonalizedImageType" => "DeveloperDiskImage"
    };
    send_plist(sock, &req_ids)?;
    let resp_ids = recv_plist(sock).context("QueryPersonalizationIdentifiers")?;
    let ids = resp_ids.as_dictionary()
        .and_then(|d| d.get("PersonalizationIdentifiers"))
        .and_then(|v| v.as_dictionary())
        .ok_or_else(|| anyhow::anyhow!("no PersonalizationIdentifiers in response"))?
        .clone();

    let board_id = int_from_plist(&ids, "BoardId")?;
    let chip_id  = int_from_plist(&ids, "ChipID")?;

    // 6. QueryNonce
    let req_nonce = plist_dict! {
        "Command"               => "QueryNonce",
        "PersonalizedImageType" => "DeveloperDiskImage"
    };
    send_plist(sock, &req_nonce)?;
    let resp_nonce = recv_plist(sock).context("QueryNonce")?;
    let nonce = resp_nonce.as_dictionary()
        .and_then(|d| d.get("PersonalizationNonce"))
        .and_then(|v| if let Value::Data(b) = v { Some(b.clone()) } else { None })
        .ok_or_else(|| anyhow::anyhow!("no PersonalizationNonce"))?;

    // ECID from lockdown
    let ecid: u64 = session.lockdown()
        .get_value(None, "UniqueChipID")
        .context("get UniqueChipID")?
        .as_unsigned_integer()
        .ok_or_else(|| anyhow::anyhow!("UniqueChipID not integer"))?;

    // 7. TSS
    eprintln!("[mounter] requesting IM4M from gs.apple.com/TSS…");
    let im4m = tss_request(manifest_bytes, board_id, chip_id, ecid, &nonce, &ids)
        .context("TSS request")?;

    // Reconnect (some iOS versions close the connection after QueryNonce)
    *sock = session.connect_rsd_shim(
            "com.apple.mobile.mobile_image_mounter.shim.remote")
        .context("reconnect mounter shim (post-TSS)")?;

    Ok(im4m)
}

fn tss_request(
    manifest_bytes: &[u8],
    board_id:       u64,
    chip_id:        u64,
    ecid:           u64,
    nonce:          &[u8],
    ids:            &plist::Dictionary,
) -> Result<Vec<u8>> {
    // Parse BuildManifest and find the matching BuildIdentity
    let mfst: Value = plist::from_bytes(manifest_bytes)
        .context("parse BuildManifest.plist")?;
    let identities = mfst.as_dictionary()
        .and_then(|d| d.get("BuildIdentities"))
        .and_then(|v| v.as_array())
        .ok_or_else(|| anyhow::anyhow!("no BuildIdentities in manifest"))?;

    let identity = identities.iter().find(|id| {
        let d = id.as_dictionary();
        let bid = d.and_then(|d| d.get("ApBoardID"))
            .and_then(|v| v.as_string())
            .and_then(parse_hex_or_dec);
        let cid = d.and_then(|d| d.get("ApChipID"))
            .and_then(|v| v.as_string())
            .and_then(parse_hex_or_dec);
        bid == Some(board_id) && cid == Some(chip_id)
    }).ok_or_else(|| anyhow::anyhow!(
        "no BuildIdentity for board_id={board_id:#x} chip_id={chip_id:#x}"
    ))?;

    let manifest_entries = identity.as_dictionary()
        .and_then(|d| d.get("Manifest"))
        .and_then(|v| v.as_dictionary())
        .ok_or_else(|| anyhow::anyhow!("no Manifest in BuildIdentity"))?;

    // Build TSS request
    let random_uuid = {
        let mut b = [0u8; 16];
        for (i, byt) in b.iter_mut().enumerate() {
            *byt = ((ecid >> (i % 8)) ^ (chip_id >> (i % 4))) as u8;
        }
        format!("{:08X}-{:04X}-{:04X}-{:04X}-{:012X}",
            u32::from_le_bytes(b[0..4].try_into().unwrap()),
            u16::from_le_bytes(b[4..6].try_into().unwrap()),
            u16::from_le_bytes(b[6..8].try_into().unwrap()),
            u16::from_le_bytes(b[8..10].try_into().unwrap()),
            u64::from_le_bytes({let mut x = [0u8;8]; x[..6].copy_from_slice(&b[10..16]); x}),
        )
    };

    let mut req = plist::Dictionary::new();
    req.insert("@HostPlatformInfo".into(), Value::String("mac".into()));
    req.insert("@VersionInfo".into(),      Value::String("libauthinstall-1104.0.9".into()));
    req.insert("@UUID".into(),             Value::String(random_uuid));
    req.insert("@ApImg4Ticket".into(),     Value::Boolean(true));
    req.insert("@BBTicket".into(),         Value::Boolean(true));
    req.insert("ApBoardID".into(),         Value::Integer((board_id as i64).into()));
    req.insert("ApChipID".into(),          Value::Integer((chip_id  as i64).into()));
    req.insert("ApECID".into(),            Value::Integer((ecid     as i64).into()));
    req.insert("ApNonce".into(),           Value::Data(nonce.to_vec()));
    req.insert("ApSecurityDomain".into(),  Value::Integer(1.into()));
    req.insert("ApProductionMode".into(),  Value::Boolean(true));
    req.insert("ApSecurityMode".into(),    Value::Boolean(true));
    req.insert("SepNonce".into(),          Value::Data(vec![0u8; 20]));
    req.insert("UID_MODE".into(),          Value::Boolean(false));

    // Forward Ap,* keys from PersonalizationIdentifiers
    for (k, v) in ids {
        if k.starts_with("Ap,") {
            req.insert(k.clone(), v.clone());
        }
    }

    // Add manifest components (only Trusted=true entries)
    for (component, entry_val) in manifest_entries {
        if component == "Info" { continue; }
        let entry = match entry_val.as_dictionary() { Some(d) => d, None => continue };
        let trusted = entry.get("Trusted")
            .and_then(|v| v.as_boolean())
            .unwrap_or(false);
        if !trusted { continue; }

        // Build entry: Digest + RestoreRequestRules result (skip Info sub-dict)
        let mut tss_entry = plist::Dictionary::new();
        if let Some(digest) = entry.get("Digest") {
            tss_entry.insert("Digest".into(), digest.clone());
        }
        tss_entry.insert("Trusted".into(), Value::Boolean(true));

        // Apply RestoreRequestRules for production devices
        // (ApProductionMode=true, ApSecurityMode=true, ApRequiresImage4=true)
        if let Some(rules) = entry.get("Info")
            .and_then(|v| v.as_dictionary())
            .and_then(|d| d.get("RestoreRequestRules"))
            .and_then(|v| v.as_array())
        {
            apply_restore_request_rules(&mut tss_entry, rules);
        }

        req.insert(component.clone(), Value::Dictionary(tss_entry));
    }

    // Serialize and POST
    let mut body = Vec::new();
    plist::to_writer_xml(&mut body, &Value::Dictionary(req))?;

    let resp = ureq::post("http://gs.apple.com/TSS/controller?action=2")
        .set("Content-Type", "text/xml; charset=\"utf-8\"")
        .set("User-Agent",   "InetURL/1.0")
        .set("Cache-Control","no-cache")
        .send_bytes(&body)
        .context("TSS POST")?;

    let resp_body = {
        let mut s = String::new();
        resp.into_reader().read_to_string(&mut s)?;
        s
    };

    // Parse: STATUS=0&MESSAGE=SUCCESS&REQUEST_STRING=<plist>
    let plist_str = resp_body.split("REQUEST_STRING=")
        .nth(1)
        .ok_or_else(|| anyhow::anyhow!("TSS response missing REQUEST_STRING: {resp_body}"))?;
    let tss_response: Value = plist::from_bytes(plist_str.as_bytes())
        .context("parse TSS response plist")?;
    let im4m = tss_response.as_dictionary()
        .and_then(|d| d.get("ApImg4Ticket"))
        .and_then(|v| if let Value::Data(b) = v { Some(b.clone()) } else { None })
        .ok_or_else(|| anyhow::anyhow!("no ApImg4Ticket in TSS response"))?;

    Ok(im4m)
}

fn apply_restore_request_rules(entry: &mut plist::Dictionary, rules: &[Value]) {
    for rule in rules {
        let rule = match rule.as_dictionary() { Some(d) => d, None => continue };
        let conditions = rule.get("Conditions").and_then(|v| v.as_dictionary());
        let actions    = rule.get("Actions").and_then(|v| v.as_array());

        let all_met = conditions.map(|conds| {
            conds.iter().all(|(k, v)| match k.as_str() {
                "ApRawProductionMode" | "ApCurrentProductionMode" =>
                    v.as_boolean() == Some(true),   // production device
                "ApRawSecurityMode" =>
                    v.as_boolean() == Some(true),   // secure mode
                "ApRequiresImage4" =>
                    v.as_boolean() == Some(true),   // always true on modern devices
                _ => true,  // unknown condition: pass
            })
        }).unwrap_or(true);

        if all_met {
            if let Some(actions) = actions {
                for action in actions {
                    if let Some(d) = action.as_dictionary() {
                        for (k, v) in d {
                            entry.insert(k.clone(), v.clone());
                        }
                    }
                }
            }
        }
    }
}

// ── plist socket protocol (4-byte BE length prefix) ───────────────────────────

fn send_plist(sock: &mut ios_rs::usbmux::MuxSocket, v: &Value) -> Result<()> {
    let mut buf = Vec::new();
    plist::to_writer_xml(&mut buf, v)?;
    let len = buf.len() as u32;
    sock.write_all(&len.to_be_bytes())?;
    sock.write_all(&buf)?;
    sock.flush()?;
    Ok(())
}

fn recv_plist(sock: &mut ios_rs::usbmux::MuxSocket) -> Result<Value> {
    let mut len_buf = [0u8; 4];
    sock.read_exact(&mut len_buf)?;
    let n = u32::from_be_bytes(len_buf) as usize;
    if n == 0 || n > 64 * 1024 * 1024 {
        bail!("recv_plist: implausible length {n}");
    }
    let mut body = vec![0u8; n];
    sock.read_exact(&mut body)?;
    Ok(plist::from_bytes(&body)?)
}

// ── helpers ───────────────────────────────────────────────────────────────────

fn resp_image_present(resp: &Value) -> bool {
    resp.as_dictionary()
        .and_then(|d| d.get("ImagePresent"))
        .and_then(|v| v.as_boolean())
        .unwrap_or(false)
}

fn int_from_plist(d: &plist::Dictionary, key: &str) -> Result<u64> {
    d.get(key)
        .and_then(|v| v.as_unsigned_integer())
        .ok_or_else(|| anyhow::anyhow!("missing or non-integer key {key:?}"))
}

fn parse_hex_or_dec(s: &str) -> Option<u64> {
    let s = s.trim();
    if s.starts_with("0x") || s.starts_with("0X") {
        u64::from_str_radix(&s[2..], 16).ok()
    } else {
        s.parse().ok()
    }
}

// ── plist_dict! macro helper ──────────────────────────────────────────────────

macro_rules! plist_dict {
    ($($k:expr => $v:expr),* $(,)?) => {{
        let mut d = plist::Dictionary::new();
        $( d.insert($k.into(), plist::Value::from($v)); )*
        plist::Value::Dictionary(d)
    }};
}
use plist_dict;
