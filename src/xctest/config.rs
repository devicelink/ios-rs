/// XCTestConfiguration NSKeyedArchiver encoding.
///
/// Mirrors go-ios `nskeyedarchiver.NewXCTestConfiguration` + `archiveXcTestConfiguration`.
/// For iOS 17+, the configuration is sent in-memory as a response to
/// `_XCT_testRunnerReadyWithCapabilities:`, not written to the device.
use plist::{Dictionary, Uid, Value};

pub struct XCTestConfigArgs<'a> {
    pub session_id:              &'a [u8; 16],
    pub test_bundle_path:        &'a str,
    pub product_module_name:     &'a str,
    pub target_bundle_id:        &'a str,
    pub target_path:             &'a str,
    pub tests_to_run:            &'a [String],
    pub tests_to_skip:           &'a [String],
    pub is_xctest:               bool,
    pub initialize_for_ui:       bool,
    /// Map of bundleID → on-device .app path for all apps involved in the test.
    /// Xcode sets this to tell XCUITest the app landscape; prevents auto-launch.
    pub app_dependencies:        &'a [(String, String)],
    /// Bundle ID of the test runner app itself.
    pub runner_bundle_id:        &'a str,
}

// Re-export for use in mod.rs

pub fn build_xctest_configuration_bytes(args: XCTestConfigArgs<'_>) -> Vec<u8> {
    let mut objects: Vec<Value> = vec![Value::String("$null".into())];

    // Class definitions (added first so we have stable UIDs)
    let xctestconfig_class = push_class(&mut objects, "XCTestConfiguration", &["NSObject"]);
    let nsuuid_class        = push_class(&mut objects, "NSUUID", &["NSObject"]);
    let nsurl_class         = push_class(&mut objects, "NSURL", &["NSObject"]);
    let nsset_class         = push_class(&mut objects, "NSSet", &["NSObject"]);
    let nsdict_class        = push_class(&mut objects, "NSDictionary", &["NSObject"]);
    let xctcaps_class       = push_class(&mut objects, "XCTCapabilities", &["NSObject"]);
    let nsmarray_class      = push_class(&mut objects, "NSMutableArray", &["NSArray", "NSObject"]);

    // Session UUID
    let session_uid = {
        let uid_idx = Uid::new(objects.len() as u64);
        let mut d = Dictionary::new();
        d.insert("NS.uuidbytes".into(), Value::Data(args.session_id.to_vec()));
        d.insert("$class".into(), Value::Uid(nsuuid_class));
        objects.push(Value::Dictionary(d));
        uid_idx
    };

    // Test bundle URL
    let bundle_url_uid = {
        let str_idx = Uid::new(objects.len() as u64);
        objects.push(Value::String(format!("file://{}", args.test_bundle_path)));
        let uid_idx = Uid::new(objects.len() as u64);
        let mut d = Dictionary::new();
        d.insert("NS.base".into(),     Value::Uid(Uid::new(0)));
        d.insert("NS.relative".into(), Value::Uid(str_idx));
        d.insert("$class".into(),      Value::Uid(nsurl_class));
        objects.push(Value::Dictionary(d));
        uid_idx
    };

    // Product module name string
    let module_name_uid = push_str(&mut objects, args.product_module_name);

    // Tests to run / skip as NSSet
    let tests_to_run_uid = if args.tests_to_run.is_empty() {
        Uid::new(0) // null = run all
    } else {
        push_nsset(&mut objects, nsset_class, args.tests_to_run)
    };
    let tests_to_skip_uid = if args.tests_to_skip.is_empty() {
        Uid::new(0)
    } else {
        push_nsset(&mut objects, nsset_class, args.tests_to_skip)
    };

    // aggregateStatisticsBeforeCrash — empty dict
    let agg_stats_uid = {
        let inner_idx = Uid::new(objects.len() as u64);
        let mut inner = Dictionary::new();
        inner.insert("XCSuiteRecordsKey".into(), Value::Dictionary(Dictionary::new()));
        inner.insert("$class".into(), Value::Uid(nsdict_class));
        objects.push(Value::Dictionary(inner));
        inner_idx
    };

    // automationFrameworkPath
    let automation_path_uid = push_str(&mut objects,
        "/System/Developer/Library/PrivateFrameworks/XCTAutomationSupport.framework");

    // IDECapabilities — updated to match Xcode 16 exactly
    let ide_caps_uid = {
        // Capabilities with value 1 (bool-like)
        let cap_keys_1 = [
            "XCTIssue capability",
            "daemon container sandbox extension",
            "delayed attachment transfer",
            "expected failure test capability",
            "in-process test parallelization capability",
            "request diagnostics for specific devices",
            "runner plan",
            "skipped suite capability",
            "skipped test capability",
            "test case run configurations",
            "test timeout capability",
            "traits",
            "ubiquitous test identifiers",
            "unified issue recorded",
        ];
        let true_uid = Uid::new(objects.len() as u64);
        objects.push(Value::Integer(1.into()));
        let two_uid = Uid::new(objects.len() as u64);
        objects.push(Value::Integer(2.into()));

        let mut key_uids = Vec::new();
        let mut val_uids = Vec::new();
        for k in &cap_keys_1 {
            key_uids.push({ let u = Uid::new(objects.len() as u64); objects.push(Value::String((*k).into())); u });
            val_uids.push(true_uid);
        }
        // "test iterations" = 2
        key_uids.push({ let u = Uid::new(objects.len() as u64); objects.push(Value::String("test iterations".into())); u });
        val_uids.push(two_uid);

        let nsdict_uid = Uid::new(objects.len() as u64);
        let mut nd = Dictionary::new();
        nd.insert("NS.keys".into(),    Value::Array(key_uids.iter().map(|u| Value::Uid(*u)).collect()));
        nd.insert("NS.objects".into(), Value::Array(val_uids.iter().map(|u| Value::Uid(*u)).collect()));
        nd.insert("$class".into(),     Value::Uid(nsdict_class));
        objects.push(Value::Dictionary(nd));

        let caps_uid = Uid::new(objects.len() as u64);
        let mut d = Dictionary::new();
        d.insert("capabilities-dictionary".into(), Value::Uid(nsdict_uid));
        d.insert("$class".into(), Value::Uid(xctcaps_class));
        objects.push(Value::Dictionary(d));
        caps_uid
    };

    // testApplicationDependencies: {bundleID → appPath} dict.
    // Xcode always sets this with all involved apps. Without it, XCUITest may
    // auto-launch a host app trying to discover the app landscape.
    let app_deps_uid = {
        let mut key_uids = Vec::new();
        let mut val_uids = Vec::new();
        for (bid, path) in args.app_dependencies {
            key_uids.push(push_str(&mut objects, bid));
            val_uids.push(push_str(&mut objects, path));
        }
        let nd_uid = Uid::new(objects.len() as u64);
        let mut nd = Dictionary::new();
        nd.insert("NS.keys".into(),    Value::Array(key_uids.iter().map(|u| Value::Uid(*u)).collect()));
        nd.insert("NS.objects".into(), Value::Array(val_uids.iter().map(|u| Value::Uid(*u)).collect()));
        nd.insert("$class".into(),     Value::Uid(nsdict_class));
        objects.push(Value::Dictionary(nd));
        nd_uid
    };

    // Target app info (optional)
    let target_bundle_uid = if !args.target_bundle_id.is_empty() {
        push_str(&mut objects, args.target_bundle_id)
    } else { Uid::new(0) };
    let target_path_uid = if !args.target_path.is_empty() {
        push_str(&mut objects, args.target_path)
    } else { Uid::new(0) };

    // The XCTestConfiguration object itself
    let config_uid = Uid::new(objects.len() as u64);
    let mut cfg = Dictionary::new();
    cfg.insert("$class".into(), Value::Uid(xctestconfig_class));
    cfg.insert("aggregateStatisticsBeforeCrash".into(), Value::Uid(agg_stats_uid));
    cfg.insert("automationFrameworkPath".into(), Value::Uid(automation_path_uid));
    cfg.insert("baselineFileRelativePath".into(), Value::Uid(Uid::new(0)));
    cfg.insert("baselineFileURL".into(),           Value::Uid(Uid::new(0)));
    cfg.insert("defaultTestExecutionTimeAllowance".into(), Value::Uid(Uid::new(0)));
    cfg.insert("disablePerformanceMetrics".into(), Value::Boolean(false));
    cfg.insert("emitOSLogs".into(),                Value::Boolean(false));
    cfg.insert("gatherLocalizableStringsData".into(), Value::Boolean(false));
    cfg.insert("initializeForUITesting".into(),    Value::Boolean(args.initialize_for_ui));
    cfg.insert("maximumTestExecutionTimeAllowance".into(), Value::Uid(Uid::new(0)));
    cfg.insert("randomExecutionOrderingSeed".into(), Value::Uid(Uid::new(0)));
    cfg.insert("reportActivities".into(),          Value::Boolean(true));
    cfg.insert("reportResultsToIDE".into(),        Value::Boolean(true));
    cfg.insert("sessionIdentifier".into(),         Value::Uid(session_uid));
    cfg.insert("systemAttachmentLifetime".into(),  Value::Integer(2.into())); // deleteAlways
    cfg.insert("testApplicationUserOverrides".into(), Value::Uid(Uid::new(0)));
    cfg.insert("testBundleRelativePath".into(),    Value::Uid(Uid::new(0)));
    cfg.insert("testBundleURL".into(),             Value::Uid(bundle_url_uid));
    cfg.insert("testExecutionOrdering".into(),     Value::Integer(0.into()));
    cfg.insert("testTimeoutsEnabled".into(),       Value::Boolean(false));
    cfg.insert("testsDrivenByIDE".into(),          Value::Boolean(false));
    cfg.insert("testsMustRunOnMainThread".into(),  Value::Boolean(true));
    cfg.insert("treatMissingBaselinesAsFailures".into(), Value::Boolean(false));
    cfg.insert("userAttachmentLifetime".into(),    Value::Integer(1.into())); // keepAlways
    cfg.insert("preferredScreenCaptureFormat".into(), Value::Integer(2.into()));
    cfg.insert("IDECapabilities".into(),               Value::Uid(ide_caps_uid));
    cfg.insert("productModuleName".into(),             Value::Uid(module_name_uid));
    cfg.insert("testApplicationDependencies".into(),   Value::Uid(app_deps_uid));

    if tests_to_run_uid != Uid::new(0) {
        cfg.insert("testsToRun".into(), Value::Uid(tests_to_run_uid));
    }
    if tests_to_skip_uid != Uid::new(0) {
        cfg.insert("testsToSkip".into(), Value::Uid(tests_to_skip_uid));
    }
    if target_bundle_uid != Uid::new(0) {
        cfg.insert("targetApplicationBundleID".into(), Value::Uid(target_bundle_uid));
    }
    if target_path_uid != Uid::new(0) {
        cfg.insert("targetApplicationPath".into(), Value::Uid(target_path_uid));
    }
    objects.push(Value::Dictionary(cfg));
    let _ = nsmarray_class; // suppress unused warning

    let mut top = Dictionary::new();
    top.insert("root".into(), Value::Uid(config_uid));
    let mut root = Dictionary::new();
    root.insert("$version".into(),  Value::Integer(100000.into()));
    root.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
    root.insert("$top".into(),      Value::Dictionary(top));
    root.insert("$objects".into(),  Value::Array(objects));

    let mut buf = Vec::new();
    plist::to_writer_binary(&mut buf, &Value::Dictionary(root)).unwrap();
    buf
}

fn push_class(objects: &mut Vec<Value>, classname: &str, supers: &[&str]) -> Uid {
    let idx = Uid::new(objects.len() as u64);
    let mut classes: Vec<Value> = vec![Value::String(classname.into())];
    classes.extend(supers.iter().map(|s| Value::String((*s).into())));
    let mut d = Dictionary::new();
    d.insert("$classname".into(), Value::String(classname.into()));
    d.insert("$classes".into(),   Value::Array(classes));
    objects.push(Value::Dictionary(d));
    idx
}

fn push_str(objects: &mut Vec<Value>, s: &str) -> Uid {
    let idx = Uid::new(objects.len() as u64);
    objects.push(Value::String(s.into()));
    idx
}

fn push_nsset(objects: &mut Vec<Value>, set_class: Uid, items: &[String]) -> Uid {
    let idx = Uid::new(objects.len() as u64);
    let ns_objects: Vec<Value> = items.iter().map(|s| Value::String(s.into())).collect();
    let mut d = Dictionary::new();
    d.insert("NS.objects".into(), Value::Array(ns_objects));
    d.insert("$class".into(),     Value::Uid(set_class));
    objects.push(Value::Dictionary(d));
    idx
}
