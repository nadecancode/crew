use crew_protocol::asides::{AsideAdmissionFailure, AsideAsk, AsideReceipt};
use crew_protocol::docs::*;
use crew_protocol::features::*;
use crew_protocol::usage_metrics::{
    MetricCoverage, UsageCounters, UsageEpicFilter, UsageTaskFilter,
};
use serde_json::{json, Value};

fn snapshot() -> Value {
    json!({
        "epoch":"docs-1","snapshot_id":"snapshot-1",
        "version":{"id":"version-1","document":{"doc_id":"doc-1","source_id":"source-1","source_generation":"1"},
            "kind":"working","markdown_hash":"hash-1","bytes":1,"observed_at_ms":10,"recorded_at_ms":null,
            "commit_id":null,"change_id":null,"author":null,"subject":null},
        "markdown":"x","blocks":[{"block_key":"block-1","start_line":1,"end_line":1,
            "block":{"kind":"paragraph","children":[{"kind":"text","text":"x"}]}}],
        "outline":[],"images":[],"render_hash":"hash-2","notices":[]
    })
}

#[test]
fn wide_values_are_exact_canonical_strings_and_null_is_not_zero() {
    for n in [0, 9_007_199_254_740_993, u64::MAX as u128 + 1, u128::MAX] {
        let encoded = serde_json::to_string(&Wide(n)).unwrap();
        assert_eq!(encoded, format!("\"{n}\""));
        assert_eq!(serde_json::from_str::<Wide>(&encoded).unwrap(), Wide(n));
    }
    for invalid in [
        json!(0),
        json!(true),
        json!(""),
        json!("00"),
        json!("01"),
        json!("+1"),
        json!("-1"),
        json!("1e3"),
        json!("1.5"),
        json!("340282366920938463463374607431768211456"),
    ] {
        assert!(serde_json::from_value::<Wide>(invalid).is_err());
    }
    let counters = UsageCounters {
        cache_read: Some(Wide(0)),
        ..UsageCounters::default()
    };
    let encoded = serde_json::to_value(counters).unwrap();
    assert_eq!(encoded["cache_read"], "0");
    assert!(encoded["cache_write"].is_null());
    assert!(Fraction::new(f64::NAN).is_err());
    assert!(Fraction::new(f64::INFINITY).is_err());
    assert!(Timestamp::new(Timestamp::MAX + 1).is_err());
}

#[test]
fn feature_maps_preserve_unknown_names_without_enabling_them() {
    let negotiation: FeaturesNegotiate =
        serde_json::from_value(json!({"versions":{"epics":1,"future":7}})).unwrap();
    assert_eq!(negotiation.versions["future"], 7);
    assert_eq!(
        serde_json::to_value(&negotiation).unwrap(),
        json!({"versions":{"epics":1,"future":7}})
    );
    let mut many = serde_json::Map::new();
    for i in 0..17 {
        many.insert(format!("f{i}"), json!(1));
    }
    assert!(serde_json::from_value::<FeaturesNegotiate>(json!({"versions":many})).is_err());
    assert!(serde_json::from_value::<FeaturesNegotiate>(json!({"versions":{"":1}})).is_err());
}

#[test]
fn whole_frame_budget_counts_escaping_and_envelope() {
    let data = Text::<MAX_MARKDOWN_BYTES>::new("\0".repeat(MAX_MARKDOWN_BYTES)).unwrap();
    assert!(encode_frame(&json!({"id":1,"result":{"markdown":data}})).is_err());
    let normal = encode_frame(&json!({"id":1,"result":{"text":"λ"}})).unwrap();
    assert_eq!(*normal.last().unwrap(), b'\n');
    assert!(normal.len() < crew_protocol::MAX_FRAME_BYTES);
    assert!(serde_json::from_str::<List<u32, 2>>("[1,2,3]").is_err());
    assert!(Text::<2>::new("éx").is_err());
}

#[test]
fn doc_tree_is_closed_and_rejects_excess_depth_nodes_and_image_references() {
    let base = snapshot();
    let parsed: DocSnapshot = serde_json::from_value(base.clone()).unwrap();
    assert_eq!(serde_json::to_value(parsed).unwrap(), base);
    let mut raw = snapshot();
    raw["blocks"][0]["block"] = json!({"kind":"html","html":"<script>bad</script>"});
    assert!(serde_json::from_value::<DocSnapshot>(raw).is_err());
    let mut block = json!({"kind":"rule"});
    for _ in 0..MAX_AST_DEPTH {
        block = json!({"kind":"quote","blocks":[block]});
    }
    let mut raw = snapshot();
    raw["blocks"][0]["block"] = block;
    assert!(serde_json::from_value::<DocSnapshot>(raw).is_err());
    let mut raw = snapshot();
    raw["blocks"][0]["block"]["children"] =
        json!(vec![json!({"kind":"soft_break"}); MAX_AST_NODES]);
    assert!(serde_json::from_value::<DocSnapshot>(raw).is_err());
    let mut raw = snapshot();
    raw["blocks"][0]["block"]["children"] = json!([{"kind":"image","alt":"x","asset":0}]);
    assert!(serde_json::from_value::<DocSnapshot>(raw).is_err());
}

#[test]
fn document_urls_rasters_and_declared_bytes_stay_bounded() {
    for url in [
        "javascript:alert(1)",
        "data:image/png,xx",
        "//example.com",
        "https:///bad",
        "https://example.com\\bad",
        "https://example.com\n",
    ] {
        assert!(serde_json::from_value::<ExternalHref>(json!(url)).is_err());
    }
    assert!(
        serde_json::from_value::<ExternalHref>(json!("https://example.com/doc#heading")).is_ok()
    );
    assert!(serde_json::from_value::<RasterType>(json!("svg")).is_err());
    let mut raw = snapshot();
    raw["version"]["bytes"] = json!(2);
    assert!(serde_json::from_value::<DocSnapshot>(raw).is_err());
    let mut raw = snapshot();
    raw["images"] = json!([{"asset_id":"asset-1","relative_path":"image.png","hash":"h","media_type":"png","bytes":10,"width":8192,"height":8192,"alt":"","state":"ready","reason":null,"safe_external_href":null}]);
    assert!(serde_json::from_value::<DocSnapshot>(raw).is_err());
}

#[test]
fn receipts_cannot_store_bodies_and_request_targets_are_captured() {
    let mut receipt = json!({"workspace_run_id":"run-1","request_id":"r-1","aside_id":"a-1","created_at":12,"outcome":"removed"});
    assert!(serde_json::from_value::<AsideReceipt>(receipt.clone()).is_ok());
    receipt["question"] = json!("private");
    assert!(serde_json::from_value::<AsideReceipt>(receipt).is_err());
    let ask: AsideAsk = serde_json::from_value(
        json!({"workspace_run_id":"run-1","request_id":"r-1","question":"synthetic","targets":[]}),
    )
    .unwrap();
    assert_eq!(ask.validate(), Err(AsideAdmissionFailure::EmptyTargets));
    let ask: AsideAsk=serde_json::from_value(json!({"workspace_run_id":"run-1","request_id":"r-1","question":"synthetic","targets":[{"agent_id":"worker","incarnation":"old"},{"agent_id":"worker","incarnation":"new"}]})).unwrap();
    assert_eq!(ask.validate(), Err(AsideAdmissionFailure::DuplicateTarget));
}

#[test]
fn known_empty_work_and_unknown_work_have_distinct_wire_values() {
    let no_epic = serde_json::to_value(UsageEpicFilter::NoEpic).unwrap();
    let no_task = serde_json::to_value(UsageEpicFilter::NoTask).unwrap();
    let unknown = serde_json::to_value(UsageEpicFilter::NotAttributed).unwrap();
    assert_ne!(no_epic, no_task);
    assert_ne!(no_task, unknown);
    assert_ne!(no_epic, unknown);
    assert_ne!(
        serde_json::to_value(UsageTaskFilter::NoTask).unwrap(),
        serde_json::to_value(UsageTaskFilter::NotAttributed).unwrap()
    );
}

#[test]
fn historical_expected_membership_and_unavailable_image_causes_are_explicit() {
    let coverage = serde_json::to_value(MetricCoverage::default()).unwrap();
    assert!(coverage["expected_agents"].is_null());
    assert!(coverage["source_backlog_files"].is_null());
    for reason in [
        DocImageReason::Unsupported(UnsupportedImageKind::Svg),
        DocImageReason::Unsupported(UnsupportedImageKind::Animated),
        DocImageReason::Blocked(BlockedImageKind::OutsideSource),
        DocImageReason::Blocked(BlockedImageKind::NotAllowed),
        DocImageReason::TooLarge(ImageLimitKind::DocumentBudget),
        DocImageReason::TooLarge(ImageLimitKind::AxisPixels),
    ] {
        assert_eq!(
            serde_json::from_value::<DocImageReason>(serde_json::to_value(reason).unwrap())
                .unwrap(),
            reason
        );
    }
    let mut raw = snapshot();
    raw["version"]["kind"] = json!("recorded");
    assert!(serde_json::from_value::<DocSnapshot>(raw.clone()).is_err());
    raw["version"]["commit_id"] = json!("commit-1");
    assert!(serde_json::from_value::<DocSnapshot>(raw).is_ok());
}

#[test]
fn asset_chunks_cannot_overstate_sizes_or_make_no_progress() {
    let valid = json!({"snapshot_id":"snapshot-1","asset_id":"asset-1","hash":"hash-1",
        "media_type":"png","offset":0,"total_bytes":1,"data_base64":"eA==","next_offset":null});
    let chunk: DocAssetChunk = serde_json::from_value(valid.clone()).unwrap();
    assert_eq!(serde_json::to_value(chunk).unwrap(), valid);
    for (key, value) in [
        ("total_bytes", json!(3_000_000)),
        ("offset", json!(1)),
        ("data_base64", json!("eB==")),
        ("data_base64", json!("")),
        ("next_offset", json!(1)),
    ] {
        let mut bad = valid.clone();
        bad[key] = value;
        assert!(serde_json::from_value::<DocAssetChunk>(bad).is_err());
    }
}
