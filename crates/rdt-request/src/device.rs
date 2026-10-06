// Adapted from redlib-org/redlib src/oauth.rs; AGPL-3.0-only. See NOTICE.
use super::oauth_resources::ANDROID_APP_VERSION_LIST;
use std::collections::HashMap;
use tegen::tegen::TextGenerator;

pub(super) fn headers() -> HashMap<String, String> {
    // Generate uuid
    let uuid = uuid::Uuid::new_v4().to_string();

    // Generate random user-agent
    let android_app_version =
        ANDROID_APP_VERSION_LIST[fastrand::usize(..ANDROID_APP_VERSION_LIST.len())].to_string();
    let android_version = fastrand::u8(9..=14);

    let android_user_agent = format!("Reddit/{android_app_version}/Android {android_version}");

    let qos = fastrand::u32(1000..=100_000);
    let qos: f32 = qos as f32 / 1000.0;
    let qos = format!("{qos:.3}");

    let codecs = TextGenerator::new()
        .generate("available-codecs=video/avc, video/hevc{, video/x-vnd.on2.vp9|}");

    // Android device headers
    let headers: HashMap<String, String> = HashMap::from([
        ("User-Agent".into(), android_user_agent.clone()),
        ("x-reddit-retry".into(), "algo=no-retries".into()),
        ("x-reddit-compression".into(), "1".into()),
        ("x-reddit-qos".into(), qos),
        ("x-reddit-media-codecs".into(), codecs),
        (
            "Content-Type".into(),
            "application/json; charset=UTF-8".into(),
        ),
        ("client-vendor-id".into(), uuid.clone()),
        ("X-Reddit-Device-Id".into(), uuid.clone()),
    ]);

    headers
}
