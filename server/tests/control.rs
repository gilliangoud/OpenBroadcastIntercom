use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use common::{
    pcm16_samples_to_le_bytes, AudioPacket, ClientLockoutPolicy, Codec, ControlEvent,
    ControlMessage, ControlResponse, IfbConfig, StereoConfig, TalkMode, MIX_SAMPLES_PER_FRAME,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn accepts_config_and_reports_status() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{control_addr}"))
        .await
        .unwrap();

    let config = ControlMessage::Config {
        user_id: 7,
        role: None,
        name: None,
        listen: vec![1, 2],
        tx: vec![1],
        vol: [(2, 0.5)].into(),
        talker_vol: None,
        codec: Some(Codec::Pcm16),
        opus_profile: None,
        talk_mode: None,
        priority: None,
        priority_channels: None,
        processing: Some(common::ProcessingConfig::default()),
        buttons: None,
        ifb: None,
        stereo: None,
        esp32_audio: None,
    };
    ws.send(Message::Text(serde_json::to_string(&config).unwrap()))
        .await
        .unwrap();
    let response = read_response(&mut ws).await;
    assert!(matches!(response, ControlResponse::Ack), "{response:?}");

    ws.send(Message::Text(
        serde_json::to_string(&ControlMessage::Status).unwrap(),
    ))
    .await
    .unwrap();

    let ControlResponse::Status { sessions, metrics } = read_response(&mut ws).await else {
        panic!("expected status response");
    };
    assert_eq!(sessions.len(), 1);
    assert_eq!(sessions[0].user_id, 7);
    assert_eq!(sessions[0].listen, vec![1, 2]);
    assert_eq!(sessions[0].tx, vec![1]);
    assert_eq!(sessions[0].supported_codecs, vec![Codec::Pcm16]);
    assert_eq!(sessions[0].talk_mode, TalkMode::Ptt);
    assert!(!sessions[0].regular_talk_active);
    assert!(!sessions[0].priority);
    assert_eq!(sessions[0].queue_depth, 0);
    assert_eq!(sessions[0].addr, None);
    assert_eq!(metrics.control_messages_received, 2);

    ws.send(Message::Text(
        serde_json::to_string(&ControlMessage::TalkMode {
            user_id: 7,
            mode: TalkMode::Muted,
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    assert!(matches!(read_response(&mut ws).await, ControlResponse::Ack));

    ws.send(Message::Text(
        serde_json::to_string(&ControlMessage::Status).unwrap(),
    ))
    .await
    .unwrap();

    let ControlResponse::Status { sessions, .. } = read_response(&mut ws).await else {
        panic!("expected status response");
    };
    assert_eq!(sessions[0].talk_mode, TalkMode::Muted);

    ws.send(Message::Text(
        serde_json::to_string(&ControlMessage::Priority {
            user_id: 7,
            active: true,
        })
        .unwrap(),
    ))
    .await
    .unwrap();
    assert!(matches!(read_response(&mut ws).await, ControlResponse::Ack));

    ws.send(Message::Text(
        serde_json::to_string(&ControlMessage::Status).unwrap(),
    ))
    .await
    .unwrap();

    let ControlResponse::Status { sessions, .. } = read_response(&mut ws).await else {
        panic!("expected status response");
    };
    assert!(sessions[0].priority);

    server_task.abort();
}

#[tokio::test]
async fn pushes_admin_config_updates_to_registered_client() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    let (mut client_ws, _) = tokio_tungstenite::connect_async(format!("ws://{control_addr}"))
        .await
        .unwrap();

    client_ws
        .send(Message::Text(
            serde_json::to_string(&ControlMessage::Hello {
                user_id: 7,
                requested_user_id: Some(7),
                client_uid: "test-client-7".to_string(),
                codecs: vec![Codec::Pcm16],
                buttons: Vec::new(),
                role: common::ClientRole::Client,
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        read_response(&mut client_ws).await,
        ControlResponse::Hello {
            preconfigured: false,
            ..
        }
    ));

    let (mut admin_ws, _) = tokio_tungstenite::connect_async(format!("ws://{control_addr}"))
        .await
        .unwrap();
    admin_ws
        .send(Message::Text(
            serde_json::to_string(&ControlMessage::Config {
                user_id: 7,
                role: None,
                name: None,
                listen: vec![2],
                tx: vec![3],
                vol: [(2, 0.4)].into(),
                talker_vol: None,
                codec: Some(Codec::Pcm16),
                opus_profile: None,
                talk_mode: None,
                priority: None,
                priority_channels: None,
                processing: Some(common::ProcessingConfig::default()),
                buttons: None,
                ifb: None,
                stereo: None,
                esp32_audio: None,
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        read_response(&mut admin_ws).await,
        ControlResponse::Ack
    ));

    let event = read_config_update(&mut client_ws).await;
    assert_eq!(
        event,
        ControlEvent::ConfigUpdate {
            user_id: 7,
            client_uid: "test-client-7".to_string(),
            name: String::new(),
            listen: vec![2],
            tx: vec![3],
            vol: [(2, 0.4)].into(),
            talker_vol: HashMap::new(),
            codec: Codec::Pcm16,
            opus_profile: common::OpusProfile::default(),
            talk_mode: TalkMode::Ptt,
            regular_talk_active: false,
            priority: false,
            priority_channels: Vec::new(),
            processing: common::ProcessingConfig::default(),
            buttons: Vec::new(),
            active_buttons: Vec::new(),
            active_direct_calls: Vec::new(),
            last_direct_caller: None,
            direct_call_history: Vec::new(),
            active_alerts: Vec::new(),
            recent_alerts: Vec::new(),
            emergency: None,
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: common::Esp32AudioConfig::default(),
        }
    );

    admin_ws
        .send(Message::Text(
            serde_json::to_string(&ControlMessage::TalkMode {
                user_id: 7,
                mode: TalkMode::Muted,
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        read_response(&mut admin_ws).await,
        ControlResponse::Ack
    ));

    let event = read_config_update(&mut client_ws).await;
    assert_eq!(
        event,
        ControlEvent::ConfigUpdate {
            user_id: 7,
            client_uid: "test-client-7".to_string(),
            name: String::new(),
            listen: vec![2],
            tx: vec![3],
            vol: [(2, 0.4)].into(),
            talker_vol: HashMap::new(),
            codec: Codec::Pcm16,
            opus_profile: common::OpusProfile::default(),
            talk_mode: TalkMode::Muted,
            regular_talk_active: false,
            priority: false,
            priority_channels: Vec::new(),
            processing: common::ProcessingConfig::default(),
            buttons: Vec::new(),
            active_buttons: Vec::new(),
            active_direct_calls: Vec::new(),
            last_direct_caller: None,
            direct_call_history: Vec::new(),
            active_alerts: Vec::new(),
            recent_alerts: Vec::new(),
            emergency: None,
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: common::Esp32AudioConfig::default(),
        }
    );

    admin_ws
        .send(Message::Text(
            serde_json::to_string(&ControlMessage::AudioCodec {
                user_id: 7,
                codec: Codec::Opus,
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        read_response(&mut admin_ws).await,
        ControlResponse::Error { .. }
    ));

    client_ws
        .send(Message::Text(
            serde_json::to_string(&ControlMessage::Hello {
                user_id: 7,
                requested_user_id: Some(7),
                client_uid: "test-client-7".to_string(),
                codecs: vec![Codec::Pcm16, Codec::Opus],
                buttons: Vec::new(),
                role: common::ClientRole::Client,
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        read_response(&mut client_ws).await,
        ControlResponse::Hello {
            preconfigured: true,
            ..
        }
    ));
    let _ = read_config_update(&mut client_ws).await;

    admin_ws
        .send(Message::Text(
            serde_json::to_string(&ControlMessage::AudioCodec {
                user_id: 7,
                codec: Codec::Opus,
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        read_response(&mut admin_ws).await,
        ControlResponse::Ack
    ));

    let event = read_config_update(&mut client_ws).await;
    assert_eq!(
        event,
        ControlEvent::ConfigUpdate {
            user_id: 7,
            client_uid: "test-client-7".to_string(),
            name: String::new(),
            listen: vec![2],
            tx: vec![3],
            vol: [(2, 0.4)].into(),
            talker_vol: HashMap::new(),
            codec: Codec::Opus,
            opus_profile: common::OpusProfile::default(),
            talk_mode: TalkMode::Muted,
            regular_talk_active: false,
            priority: false,
            priority_channels: Vec::new(),
            processing: common::ProcessingConfig::default(),
            buttons: Vec::new(),
            active_buttons: Vec::new(),
            active_direct_calls: Vec::new(),
            last_direct_caller: None,
            direct_call_history: Vec::new(),
            active_alerts: Vec::new(),
            recent_alerts: Vec::new(),
            emergency: None,
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: common::Esp32AudioConfig::default(),
        }
    );

    server_task.abort();
}

#[tokio::test]
async fn admin_api_preconfigures_client_before_connect() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();
    let admin_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_addr = admin_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run_with_options(
        Arc::clone(&server_socket),
        control_listener,
        server::RunOptions {
            admin_listener: Some(admin_listener),
            admin_state_file: None,
            ..Default::default()
        },
    ));

    let body = r#"{"name":"Pi Talent","listen":[2],"tx":[3],"vol":{"2":0.4},"codec":"opus","talk_mode":"muted","priority":true,"buttons":[{"id":"director","label":"Director","mode":"momentary","actions":[{"type":"transmit","channels":[8]}]}],"ifb":{"enabled":true,"program":[2],"interrupt":[8],"duck_gain":0.125}}"#;
    let (status, response_body) =
        http_request(admin_addr, "PUT", "/admin/api/clients/12", Some(body)).await;
    assert_eq!(status, 200, "{response_body}");

    let (mut client_ws, _) = tokio_tungstenite::connect_async(format!("ws://{control_addr}"))
        .await
        .unwrap();
    client_ws
        .send(Message::Text(
            serde_json::to_string(&ControlMessage::Hello {
                user_id: 12,
                requested_user_id: Some(12),
                client_uid: "test-client-12".to_string(),
                codecs: vec![Codec::Pcm16, Codec::Opus],
                buttons: vec![common::ButtonCapability {
                    id: "director".to_string(),
                    label: "Director".to_string(),
                }],
                role: common::ClientRole::Client,
            })
            .unwrap(),
        ))
        .await
        .unwrap();

    assert!(matches!(
        read_response(&mut client_ws).await,
        ControlResponse::Hello {
            preconfigured: true,
            ..
        }
    ));
    assert_eq!(
        read_event(&mut client_ws).await,
        ControlEvent::ConfigUpdate {
            user_id: 12,
            client_uid: "test-client-12".to_string(),
            name: "Pi Talent".to_string(),
            listen: vec![2],
            tx: vec![3],
            vol: [(2, 0.4)].into(),
            talker_vol: HashMap::new(),
            codec: Codec::Opus,
            opus_profile: common::OpusProfile::default(),
            talk_mode: TalkMode::Muted,
            regular_talk_active: false,
            priority: true,
            priority_channels: Vec::new(),
            processing: common::ProcessingConfig::default(),
            buttons: vec![common::TalkButtonConfig {
                id: "director".to_string(),
                label: "Director".to_string(),
                color: None,
                mode: common::TalkButtonMode::Momentary,
                actions: vec![common::TalkButtonAction::Transmit {
                    channels: vec![8],
                    users: Vec::new(),
                    duck: false,
                }],
            }],
            active_buttons: Vec::new(),
            active_direct_calls: Vec::new(),
            last_direct_caller: None,
            direct_call_history: Vec::new(),
            active_alerts: Vec::new(),
            recent_alerts: Vec::new(),
            emergency: None,
            ifb: IfbConfig {
                enabled: true,
                program: vec![2],
                interrupt: vec![8],
                duck_gain: 0.125,
            },
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: common::Esp32AudioConfig::default(),
        }
    );

    let body = r#"{"ifb":{"enabled":true,"program":[2],"interrupt":[9],"duck_gain":0.25}}"#;
    let (status, response_body) =
        http_request(admin_addr, "PATCH", "/admin/api/clients/12", Some(body)).await;
    assert_eq!(status, 200, "{response_body}");
    assert_eq!(
        read_event(&mut client_ws).await,
        ControlEvent::ConfigUpdate {
            user_id: 12,
            client_uid: "test-client-12".to_string(),
            name: "Pi Talent".to_string(),
            listen: vec![2],
            tx: vec![3],
            vol: [(2, 0.4)].into(),
            talker_vol: HashMap::new(),
            codec: Codec::Opus,
            opus_profile: common::OpusProfile::default(),
            talk_mode: TalkMode::Muted,
            regular_talk_active: false,
            priority: true,
            priority_channels: Vec::new(),
            processing: common::ProcessingConfig::default(),
            buttons: vec![common::TalkButtonConfig {
                id: "director".to_string(),
                label: "Director".to_string(),
                color: None,
                mode: common::TalkButtonMode::Momentary,
                actions: vec![common::TalkButtonAction::Transmit {
                    channels: vec![8],
                    users: Vec::new(),
                    duck: false,
                }],
            }],
            active_buttons: Vec::new(),
            active_direct_calls: Vec::new(),
            last_direct_caller: None,
            direct_call_history: Vec::new(),
            active_alerts: Vec::new(),
            recent_alerts: Vec::new(),
            emergency: None,
            ifb: IfbConfig {
                enabled: true,
                program: vec![2],
                interrupt: vec![9],
                duck_gain: 0.25,
            },
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: common::Esp32AudioConfig::default(),
        }
    );

    let body = r#"{"name":"PCM Only","listen":[1],"tx":[1],"vol":{},"codec":"opus","talk_mode":"open","priority":false}"#;
    let (status, response_body) =
        http_request(admin_addr, "PUT", "/admin/api/clients/13", Some(body)).await;
    assert_eq!(status, 200, "{response_body}");
    let (mut pcm_client_ws, _) = tokio_tungstenite::connect_async(format!("ws://{control_addr}"))
        .await
        .unwrap();
    pcm_client_ws
        .send(Message::Text(
            serde_json::to_string(&ControlMessage::Hello {
                user_id: 13,
                requested_user_id: Some(13),
                client_uid: "test-client-13".to_string(),
                codecs: vec![Codec::Pcm16],
                buttons: Vec::new(),
                role: common::ClientRole::Client,
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        read_response(&mut pcm_client_ws).await,
        ControlResponse::Hello {
            preconfigured: true,
            ..
        }
    ));
    assert_eq!(
        read_event(&mut pcm_client_ws).await,
        ControlEvent::ConfigUpdate {
            user_id: 13,
            client_uid: "test-client-13".to_string(),
            name: "PCM Only".to_string(),
            listen: vec![1],
            tx: vec![1],
            vol: HashMap::new(),
            talker_vol: HashMap::new(),
            codec: Codec::Pcm16,
            opus_profile: common::OpusProfile::default(),
            talk_mode: TalkMode::Open,
            regular_talk_active: false,
            priority: false,
            priority_channels: Vec::new(),
            processing: common::ProcessingConfig::default(),
            buttons: Vec::new(),
            active_buttons: Vec::new(),
            active_direct_calls: Vec::new(),
            last_direct_caller: None,
            direct_call_history: Vec::new(),
            active_alerts: Vec::new(),
            recent_alerts: Vec::new(),
            emergency: None,
            ifb: IfbConfig::default(),
            lockout: ClientLockoutPolicy::default(),
            stereo: StereoConfig::default(),
            esp32_audio: common::Esp32AudioConfig::default(),
        }
    );
    let (status, state_body) = http_request(admin_addr, "GET", "/admin/api/state", None).await;
    assert_eq!(status, 200, "{state_body}");
    let state: Value = serde_json::from_str(&state_body).unwrap();
    let client_12 = state["clients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|client| client["user_id"] == 12)
        .unwrap();
    assert_eq!(client_12["ifb"]["enabled"], true);
    let session_12 = state["sessions"]
        .as_array()
        .unwrap()
        .iter()
        .find(|session| session["user_id"] == 12)
        .unwrap();
    assert_eq!(session_12["ifb"]["interrupt"][0], 9);
    assert_eq!(session_12["ifb_status"]["active"], false);
    assert!(state["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning["user_id"] == 13));

    server_task.abort();
}

#[tokio::test]
async fn admin_api_serves_state_assets_and_channel_crud() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_addr = admin_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run_with_options(
        Arc::clone(&server_socket),
        control_listener,
        server::RunOptions {
            admin_listener: Some(admin_listener),
            admin_state_file: None,
            ..Default::default()
        },
    ));

    let (status, html) = http_request(admin_addr, "GET", "/admin/", None).await;
    assert_eq!(status, 200);
    assert!(html.contains("Intercom Admin"));

    let (status, js) = http_request(admin_addr, "GET", "/admin/app.js", None).await;
    assert_eq!(status, 200);
    assert!(js.contains("/admin/api"));

    let (status, html) = http_request(admin_addr, "GET", "/admin/presets/", None).await;
    assert_eq!(status, 200);
    assert!(html.contains("Presets &amp; Templates"));

    for path in [
        "/admin/clients/",
        "/admin/routing/",
        "/admin/calls/",
        "/admin/recording/",
        "/admin/system/",
    ] {
        let (status, html) = http_request(admin_addr, "GET", path, None).await;
        assert_eq!(status, 200, "{path}");
        assert!(html.contains("/admin/app.js"), "{path}");
        assert!(html.contains("/admin/style.css"), "{path}");
    }

    let (status, body) = http_request(admin_addr, "GET", "/admin/api/recording/status", None).await;
    assert_eq!(status, 200, "{body}");
    let recording_status: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(recording_status["active"], false);
    assert!(recording_status["recent_sessions"].is_array());

    let (status, body) =
        http_request(admin_addr, "GET", "/admin/api/recording/sessions", None).await;
    assert_eq!(status, 200, "{body}");
    let recording_sessions: Value = serde_json::from_str(&body).unwrap();
    assert!(recording_sessions.is_array());

    let (status, body) = http_request(
        admin_addr,
        "GET",
        "/admin/api/transcription/live/status",
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let live_status: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(live_status["active"], false);

    let (status, body) = http_request(
        admin_addr,
        "POST",
        "/admin/api/transcription/live/start",
        Some(r#"{"users":[1]}"#),
    )
    .await;
    assert_eq!(status, 400, "{body}");
    assert!(body.contains("disabled") || body.contains("transcription-whisper"));

    let (status, body) = http_request(
        admin_addr,
        "POST",
        "/admin/api/transcription/live/stop",
        None,
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let (status, body) = http_request(
        admin_addr,
        "PUT",
        "/admin/api/channels/2",
        Some(r#"{"name":"Program"}"#),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let (status, body) = http_request(admin_addr, "GET", "/admin/api/state", None).await;
    assert_eq!(status, 200, "{body}");
    let state: Value = serde_json::from_str(&body).unwrap();
    let channel = state["channels"]
        .as_array()
        .unwrap()
        .iter()
        .find(|channel| channel["id"] == 2)
        .unwrap();
    assert_eq!(channel["name"], "Program");

    let (status, body) = http_request(admin_addr, "DELETE", "/admin/api/channels/2", None).await;
    assert_eq!(status, 200, "{body}");

    let device_client_body = r#"{"client_uid":"unit-device","name":"Unit","listen":[0],"tx":[0],"vol":{},"codec":"pcm16","talk_mode":"ptt","priority":false}"#;
    let (status, body) = http_request(
        admin_addr,
        "PUT",
        "/admin/api/clients/55",
        Some(device_client_body),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let (status, body) = http_request(admin_addr, "GET", "/admin/api/state", None).await;
    assert_eq!(status, 200, "{body}");
    let state: Value = serde_json::from_str(&body).unwrap();
    assert!(state["devices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|device| device["client_uid"] == "unit-device"));

    let (status, body) =
        http_request(admin_addr, "DELETE", "/admin/api/devices/unit-device", None).await;
    assert_eq!(status, 200, "{body}");

    let (status, body) = http_request(admin_addr, "GET", "/admin/api/state", None).await;
    assert_eq!(status, 200, "{body}");
    let state: Value = serde_json::from_str(&body).unwrap();
    assert!(!state["devices"]
        .as_array()
        .unwrap()
        .iter()
        .any(|device| device["client_uid"] == "unit-device"));
    let client = state["clients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|client| client["user_id"] == 55)
        .unwrap();
    assert_eq!(client["name"], "Unit");
    assert!(client["client_uid"].is_null());

    let preset_body = r#"{"name":"Refs","clients":[{"user_id":40,"name":"Ref","listen":[1],"tx":[1],"vol":{},"talker_vol":{"41":0.8},"codec":"pcm16","talk_mode":"open","priority":false}]}"#;
    let (status, body) = http_request(
        admin_addr,
        "PUT",
        "/admin/api/presets/refs",
        Some(preset_body),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let (status, body) = http_request(admin_addr, "GET", "/admin/api/state", None).await;
    assert_eq!(status, 200, "{body}");
    let state: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(state["presets"][0]["id"], "refs");
    assert_eq!(state["presets"][0]["clients"][0]["talker_vol"]["41"], 0.8);

    let (status, body) = http_request(admin_addr, "POST", "/admin/api/presets/refs", None).await;
    assert_eq!(status, 200, "{body}");

    let (status, body) = http_request(admin_addr, "GET", "/admin/api/state", None).await;
    assert_eq!(status, 200, "{body}");
    let state: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(state["clients"][0]["user_id"], 40);
    assert_eq!(state["clients"][0]["talker_vol"]["41"], 0.8);

    let (status, body) = http_request(admin_addr, "DELETE", "/admin/api/presets/refs", None).await;
    assert_eq!(status, 200, "{body}");

    let template_body = r#"{"name":"Referee","client":{"name":"Ref","listen":[2],"tx":[1],"vol":{"2":0.7},"talker_vol":{"41":0.5},"codec":"pcm16","talk_mode":"ptt","priority":true,"buttons":[{"id":"director","label":"Director","mode":"momentary","actions":[{"type":"transmit","channels":[9],"users":[],"duck":false}]}],"ifb":{"enabled":true,"program":[2],"interrupt":[9],"duck_gain":0.125}}}"#;
    let (status, body) = http_request(
        admin_addr,
        "PUT",
        "/admin/api/templates/referee",
        Some(template_body),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let (status, body) = http_request(admin_addr, "GET", "/admin/api/state", None).await;
    assert_eq!(status, 200, "{body}");
    let state: Value = serde_json::from_str(&body).unwrap();
    let template = state["templates"]
        .as_array()
        .unwrap()
        .iter()
        .find(|template| template["id"] == "referee")
        .unwrap();
    assert_eq!(template["id"], "referee");
    assert_eq!(template["client"]["buttons"][0]["id"], "director");

    let (status, body) = http_request(
        admin_addr,
        "POST",
        "/admin/api/templates/referee/apply",
        Some(r#"{"user_id":42}"#),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let (status, body) = http_request(admin_addr, "GET", "/admin/api/state", None).await;
    assert_eq!(status, 200, "{body}");
    let state: Value = serde_json::from_str(&body).unwrap();
    let client = state["clients"]
        .as_array()
        .unwrap()
        .iter()
        .find(|client| client["user_id"] == 42)
        .unwrap();
    assert_eq!(client["name"], "Ref");
    assert_eq!(client["buttons"][0]["id"], "director");

    let (status, body) =
        http_request(admin_addr, "DELETE", "/admin/api/templates/referee", None).await;
    assert_eq!(status, 200, "{body}");

    server_task.abort();
}

#[tokio::test]
async fn admin_state_exposes_live_audio_health() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_addr = admin_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run_with_options(
        Arc::clone(&server_socket),
        control_listener,
        server::RunOptions {
            admin_listener: Some(admin_listener),
            admin_state_file: None,
            ..Default::default()
        },
    ));

    let (status, body) = http_request(
        admin_addr,
        "PUT",
        "/admin/api/clients/1",
        Some(r#"{"name":"Talker","listen":[],"tx":[1],"vol":{},"codec":"pcm48","talk_mode":"open","priority":false}"#),
    )
    .await;
    assert_eq!(status, 200, "{body}");
    let (status, body) = http_request(
        admin_addr,
        "PUT",
        "/admin/api/clients/2",
        Some(r#"{"name":"Listener","listen":[1],"tx":[],"vol":{},"codec":"pcm48","talk_mode":"muted","priority":false}"#),
    )
    .await;
    assert_eq!(status, 200, "{body}");

    let talker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    send_audio_packet(&listener, server_addr, 2, 1, 1, &[0; MIX_SAMPLES_PER_FRAME]).await;
    send_audio_packet(
        &talker,
        server_addr,
        1,
        1,
        1,
        &[i16::MAX; MIX_SAMPLES_PER_FRAME],
    )
    .await;
    let mut buf = [0_u8; 1500];
    let _ = tokio::time::timeout(Duration::from_secs(2), listener.recv(&mut buf))
        .await
        .unwrap()
        .unwrap();

    let (status, body) = http_request(admin_addr, "GET", "/admin/api/state", None).await;
    assert_eq!(status, 200, "{body}");
    let state: Value = serde_json::from_str(&body).unwrap();
    let sessions = state["sessions"].as_array().unwrap();
    let talker = sessions
        .iter()
        .find(|session| session["user_id"] == 1)
        .unwrap();
    let listener = sessions
        .iter()
        .find(|session| session["user_id"] == 2)
        .unwrap();

    assert_eq!(talker["input"]["active"], true);
    assert!(talker["input"]["rms"].as_f64().unwrap() > 0.5);
    assert!(listener["output"]["rms"].as_f64().unwrap() > 0.5);
    assert!(listener["output"]["limiter_events"].as_u64().unwrap() >= 1);
    assert_eq!(talker["transport"]["source_queue_depth"], 0);

    server_task.abort();
}

#[tokio::test]
async fn admin_api_saves_unsupported_live_codec_as_desired_with_live_fallback() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();
    let admin_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_addr = admin_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run_with_options(
        Arc::clone(&server_socket),
        control_listener,
        server::RunOptions {
            admin_listener: Some(admin_listener),
            admin_state_file: None,
            ..Default::default()
        },
    ));

    let (mut client_ws, _) = tokio_tungstenite::connect_async(format!("ws://{control_addr}"))
        .await
        .unwrap();
    client_ws
        .send(Message::Text(
            serde_json::to_string(&ControlMessage::Hello {
                user_id: 20,
                requested_user_id: Some(20),
                client_uid: "test-client-20".to_string(),
                codecs: vec![Codec::Pcm16],
                buttons: Vec::new(),
                role: common::ClientRole::Client,
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        read_response(&mut client_ws).await,
        ControlResponse::Hello {
            preconfigured: false,
            ..
        }
    ));

    let body = r#"{"name":"","listen":[1],"tx":[1],"vol":{},"codec":"opus","talk_mode":"open","priority":false}"#;
    let (status, response_body) =
        http_request(admin_addr, "PUT", "/admin/api/clients/20", Some(body)).await;

    assert_eq!(status, 200, "{response_body}");
    let desired: Value = serde_json::from_str(&response_body).unwrap();
    assert_eq!(desired["codec"], "opus");

    let ControlEvent::ConfigUpdate {
        codec,
        tx,
        talk_mode,
        ..
    } = read_config_update(&mut client_ws).await
    else {
        panic!("expected config update");
    };
    assert_eq!(codec, Codec::Pcm16);
    assert_eq!(tx, vec![1]);
    assert_eq!(talk_mode, TalkMode::Open);

    let (status, state_body) = http_request(admin_addr, "GET", "/admin/api/state", None).await;
    assert_eq!(status, 200, "{state_body}");
    let state: Value = serde_json::from_str(&state_body).unwrap();
    assert!(state["warnings"]
        .as_array()
        .unwrap()
        .iter()
        .any(|warning| warning["user_id"] == 20
            && warning["message"]
                .as_str()
                .unwrap()
                .contains("not supported")));

    server_task.abort();
}

#[tokio::test]
async fn dedicated_button_routes_audio_to_alternate_channel() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();
    let admin_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let admin_addr = admin_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run_with_options(
        Arc::clone(&server_socket),
        control_listener,
        server::RunOptions {
            admin_listener: Some(admin_listener),
            admin_state_file: None,
            ..Default::default()
        },
    ));

    let body = r#"{"name":"Ref","listen":[1],"tx":[1],"vol":{},"codec":"pcm48","talk_mode":"muted","priority":false,"buttons":[{"id":"director","label":"Director","mode":"momentary","actions":[{"type":"transmit","channels":[2],"users":[],"duck":false}]}]}"#;
    let (status, response_body) =
        http_request(admin_addr, "PUT", "/admin/api/clients/1", Some(body)).await;
    assert_eq!(status, 200, "{response_body}");
    let body = r#"{"name":"Other Ref","listen":[1],"tx":[],"vol":{},"codec":"pcm48","talk_mode":"muted","priority":false,"buttons":[]}"#;
    let (status, response_body) =
        http_request(admin_addr, "PUT", "/admin/api/clients/2", Some(body)).await;
    assert_eq!(status, 200, "{response_body}");
    let body = r#"{"name":"Director","listen":[2],"tx":[],"vol":{},"codec":"pcm48","talk_mode":"muted","priority":false,"buttons":[]}"#;
    let (status, response_body) =
        http_request(admin_addr, "PUT", "/admin/api/clients/3", Some(body)).await;
    assert_eq!(status, 200, "{response_body}");

    let talker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let regular_listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let director_listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    send_audio_packet(
        &regular_listener,
        server_addr,
        2,
        1,
        1,
        &[0; MIX_SAMPLES_PER_FRAME],
    )
    .await;
    send_audio_packet(
        &director_listener,
        server_addr,
        3,
        2,
        1,
        &[0; MIX_SAMPLES_PER_FRAME],
    )
    .await;

    send_audio_packet(
        &talker,
        server_addr,
        1,
        1,
        1,
        &[1_000; MIX_SAMPLES_PER_FRAME],
    )
    .await;
    let mut buf = [0_u8; 1500];
    assert!(
        tokio::time::timeout(Duration::from_millis(120), regular_listener.recv(&mut buf))
            .await
            .is_err()
    );

    let (mut control_ws, _) = tokio_tungstenite::connect_async(format!("ws://{control_addr}"))
        .await
        .unwrap();
    control_ws
        .send(Message::Text(
            serde_json::to_string(&ControlMessage::Button {
                user_id: 1,
                button_id: "director".to_string(),
                pressed: true,
            })
            .unwrap(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        read_response(&mut control_ws).await,
        ControlResponse::Ack
    ));

    send_audio_packet(
        &talker,
        server_addr,
        1,
        2,
        2,
        &[1_000; MIX_SAMPLES_PER_FRAME],
    )
    .await;
    tokio::time::timeout(Duration::from_secs(2), director_listener.recv(&mut buf))
        .await
        .unwrap()
        .unwrap();

    server_task.abort();
}

async fn read_response(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> ControlResponse {
    let message = ws.next().await.unwrap().unwrap();
    let Message::Text(text) = message else {
        panic!("expected text response");
    };
    serde_json::from_str(&text).unwrap()
}

async fn read_event(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> ControlEvent {
    let message = ws.next().await.unwrap().unwrap();
    let Message::Text(text) = message else {
        panic!("expected text event");
    };
    serde_json::from_str(&text).unwrap()
}

async fn read_config_update(
    ws: &mut tokio_tungstenite::WebSocketStream<
        tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
    >,
) -> ControlEvent {
    for _ in 0..10 {
        let event = read_event(ws).await;
        if matches!(event, ControlEvent::ConfigUpdate { .. }) {
            return event;
        }
    }

    panic!("expected config_update event");
}

async fn http_request(
    addr: std::net::SocketAddr,
    method: &str,
    path: &str,
    body: Option<&str>,
) -> (u16, String) {
    let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
    let body = body.unwrap_or("");
    let request = format!(
        "{method} {path} HTTP/1.1\r\nHost: {addr}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
        body.len()
    );
    stream.write_all(request.as_bytes()).await.unwrap();
    let mut response = String::new();
    stream.read_to_string(&mut response).await.unwrap();
    let (headers, body) = response.split_once("\r\n\r\n").unwrap();
    let status = headers
        .lines()
        .next()
        .unwrap()
        .split_whitespace()
        .nth(1)
        .unwrap()
        .parse::<u16>()
        .unwrap();
    (status, body.to_string())
}

async fn send_audio_packet(
    socket: &UdpSocket,
    server_addr: std::net::SocketAddr,
    user_id: u16,
    channel_id: u16,
    seq: u16,
    samples: &[i16],
) {
    let packet = AudioPacket {
        user_id,
        target: common::AudioTarget::Channel(channel_id),
        codec: Codec::Pcm48,
        seq,
        timestamp: seq as u32 * MIX_SAMPLES_PER_FRAME as u32,
        payload: pcm16_samples_to_le_bytes(samples),
    };
    let mut encoded = Vec::new();
    packet.encode(&mut encoded).unwrap();
    socket.send_to(&encoded, server_addr).await.unwrap();
}
