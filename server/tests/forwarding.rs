use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use common::{
    pcm16_le_bytes_to_samples, pcm16_samples_to_le_bytes, AudioPacket, AudioTarget, Codec,
    ControlMessage, ControlResponse, EmergencyTarget, IfbConfig, StereoConfig, TalkMode,
    MAX_PACKET_BYTES, PCM48_PAYLOAD_BYTES, PCM48_STEREO_PAYLOAD_BYTES, SAMPLES_PER_FRAME,
    SERVER_USER_ID,
};
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, UdpSocket};
use tokio_tungstenite::tungstenite::Message;

#[tokio::test]
async fn sends_mixed_audio_to_other_listener_on_same_channel() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    send_control(
        control_addr,
        basic_config(1, Vec::new(), vec![10], TalkMode::Open),
    )
    .await;
    send_control(
        control_addr,
        basic_config(2, vec![10], Vec::new(), TalkMode::Muted),
    )
    .await;

    let client_a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let client_b = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    send_packet(&client_b, server_addr, 2, 10, 1, &[0; SAMPLES_PER_FRAME]).await;
    send_packet(&client_a, server_addr, 1, 10, 2, &[123; SAMPLES_PER_FRAME]).await;

    let mut buf = [0_u8; 512];
    let len = tokio::time::timeout(Duration::from_secs(2), client_b.recv(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let packet = AudioPacket::decode(&buf[..len]).unwrap();

    assert_eq!(packet.user_id, SERVER_USER_ID);
    assert_eq!(packet.target, AudioTarget::Mixed);
    let samples = pcm16_le_bytes_to_samples(&packet.payload).unwrap();
    assert_steady_level(&samples, 123);

    server_task.abort();
}

#[tokio::test]
async fn registration_packet_makes_receive_only_client_reachable() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));

    send_control(
        control_addr,
        ControlMessage::Config {
            user_id: 2,
            role: None,
            name: None,
            listen: vec![10],
            tx: Vec::new(),
            vol: HashMap::new(),
            talker_vol: None,
            codec: Some(Codec::Pcm16),
            opus_profile: None,
            talk_mode: Some(TalkMode::Muted),
            priority: Some(false),
            priority_channels: None,
            processing: None,
            buttons: None,
            ifb: None,
            stereo: None,
            esp32_audio: None,
        },
    )
    .await;
    send_control(
        control_addr,
        basic_config(1, Vec::new(), vec![10], TalkMode::Open),
    )
    .await;

    let talker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    send_registration_packet(&listener, server_addr, 2).await;
    send_packet(&talker, server_addr, 1, 10, 1, &[321; SAMPLES_PER_FRAME]).await;

    let mut buf = [0_u8; 512];
    let len = tokio::time::timeout(Duration::from_secs(2), listener.recv(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let packet = AudioPacket::decode(&buf[..len]).unwrap();
    let samples = pcm16_le_bytes_to_samples(&packet.payload).unwrap();
    assert_steady_level(&samples, 321);

    server_task.abort();
}

#[tokio::test]
async fn pcm16_talker_reaches_pcm48_listener() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    send_control(
        control_addr,
        config_with_codec(1, Vec::new(), vec![10], TalkMode::Open, Codec::Pcm16),
    )
    .await;
    send_control(
        control_addr,
        config_with_codec(2, vec![10], Vec::new(), TalkMode::Muted, Codec::Pcm48),
    )
    .await;

    let talker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    send_registration_packet_with_codec(&listener, server_addr, 2, Codec::Pcm48).await;
    send_packet(&talker, server_addr, 1, 10, 1, &[1_234; SAMPLES_PER_FRAME]).await;

    let mut buf = vec![0_u8; MAX_PACKET_BYTES];
    let len = tokio::time::timeout(Duration::from_secs(2), listener.recv(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let packet = AudioPacket::decode(&buf[..len]).unwrap();

    assert_eq!(packet.user_id, SERVER_USER_ID);
    assert_eq!(packet.target, AudioTarget::Mixed);
    assert_eq!(packet.codec, Codec::Pcm48);
    assert_eq!(packet.payload.len(), PCM48_PAYLOAD_BYTES);
    let samples = pcm16_le_bytes_to_samples(&packet.payload).unwrap();
    assert_steady_level(&samples, 1_234);

    server_task.abort();
}

#[tokio::test]
async fn stereo_pcm48_listener_receives_full_sized_packet() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    send_control(
        control_addr,
        config_with_codec(1, Vec::new(), vec![10], TalkMode::Open, Codec::Pcm16),
    )
    .await;
    send_control(
        control_addr,
        ControlMessage::Config {
            user_id: 2,
            role: None,
            name: None,
            listen: vec![10],
            tx: Vec::new(),
            vol: HashMap::new(),
            talker_vol: None,
            codec: Some(Codec::Pcm48),
            opus_profile: None,
            talk_mode: Some(TalkMode::Muted),
            priority: None,
            priority_channels: None,
            processing: None,
            buttons: None,
            ifb: None,
            stereo: Some(StereoConfig {
                enabled: true,
                channel_pan: HashMap::new(),
            }),
            esp32_audio: None,
        },
    )
    .await;

    let talker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    send_registration_packet_with_codec(&listener, server_addr, 2, Codec::Pcm48).await;
    send_packet(&talker, server_addr, 1, 10, 1, &[2_000; SAMPLES_PER_FRAME]).await;

    let mut buf = vec![0_u8; MAX_PACKET_BYTES];
    let len = tokio::time::timeout(Duration::from_secs(2), listener.recv(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let packet = AudioPacket::decode(&buf[..len]).unwrap();

    assert_eq!(packet.codec, Codec::Pcm48);
    assert_eq!(packet.payload.len(), PCM48_STEREO_PAYLOAD_BYTES);

    server_task.abort();
}

#[tokio::test]
async fn direct_call_reaches_target_without_shared_channel() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    send_control(
        control_addr,
        basic_config(1, Vec::new(), Vec::new(), TalkMode::Open),
    )
    .await;
    send_control(
        control_addr,
        basic_config(2, Vec::new(), Vec::new(), TalkMode::Muted),
    )
    .await;
    send_control(
        control_addr,
        basic_config(3, Vec::new(), Vec::new(), TalkMode::Muted),
    )
    .await;
    send_control(
        control_addr,
        ControlMessage::DirectCall {
            user_id: 1,
            target_user_id: 2,
            active: true,
            duck: false,
        },
    )
    .await;

    let caller = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let target = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let other = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    send_registration_packet(&target, server_addr, 2).await;
    send_registration_packet(&other, server_addr, 3).await;
    send_target_packet(
        &caller,
        server_addr,
        1,
        AudioTarget::Direct(2),
        2,
        &[777; SAMPLES_PER_FRAME],
    )
    .await;

    receive_until_level(&target, 777).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(150), receive_until_level(&other, 777))
            .await
            .is_err(),
        "unrelated client received direct-call audio"
    );

    server_task.abort();
}

#[tokio::test]
async fn reply_routes_to_last_direct_caller() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    send_control(
        control_addr,
        basic_config(1, Vec::new(), Vec::new(), TalkMode::Muted),
    )
    .await;
    send_control(
        control_addr,
        basic_config(2, Vec::new(), Vec::new(), TalkMode::Muted),
    )
    .await;
    send_control(
        control_addr,
        ControlMessage::DirectCall {
            user_id: 1,
            target_user_id: 2,
            active: true,
            duck: false,
        },
    )
    .await;
    send_control(
        control_addr,
        ControlMessage::ReplyCall {
            user_id: 2,
            active: true,
            duck: false,
        },
    )
    .await;

    let caller = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let replier = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    send_registration_packet(&caller, server_addr, 1).await;
    send_target_packet(
        &replier,
        server_addr,
        2,
        AudioTarget::Direct(1),
        2,
        &[444; SAMPLES_PER_FRAME],
    )
    .await;

    receive_until_level(&caller, 444).await;
    server_task.abort();
}

#[tokio::test]
async fn direct_call_ducks_recipient_other_audio() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    send_control(
        control_addr,
        basic_config(1, Vec::new(), Vec::new(), TalkMode::Open),
    )
    .await;
    send_control(
        control_addr,
        basic_config(2, vec![10], Vec::new(), TalkMode::Muted),
    )
    .await;
    send_control(
        control_addr,
        basic_config(3, Vec::new(), vec![10], TalkMode::Open),
    )
    .await;
    send_control(
        control_addr,
        ControlMessage::DirectCall {
            user_id: 1,
            target_user_id: 2,
            active: true,
            duck: true,
        },
    )
    .await;

    let direct = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let recipient = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let program = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    send_registration_packet(&recipient, server_addr, 2).await;
    send_packet(&program, server_addr, 3, 10, 1, &[4_000; SAMPLES_PER_FRAME]).await;
    send_target_packet(
        &direct,
        server_addr,
        1,
        AudioTarget::Direct(2),
        2,
        &[4_000; SAMPLES_PER_FRAME],
    )
    .await;

    receive_until_level(&recipient, 4_500).await;
    server_task.abort();
}

#[tokio::test]
async fn mixes_multiple_talkers_for_one_listener() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    send_control(
        control_addr,
        basic_config(1, Vec::new(), vec![10], TalkMode::Open),
    )
    .await;
    send_control(
        control_addr,
        basic_config(2, vec![10], Vec::new(), TalkMode::Muted),
    )
    .await;
    send_control(
        control_addr,
        basic_config(3, Vec::new(), vec![10], TalkMode::Open),
    )
    .await;

    let client_a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let client_b = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let client_c = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    send_packet(&client_b, server_addr, 2, 10, 1, &[0; SAMPLES_PER_FRAME]).await;
    send_packet(
        &client_a,
        server_addr,
        1,
        10,
        2,
        &[1_000; SAMPLES_PER_FRAME],
    )
    .await;
    send_packet(
        &client_c,
        server_addr,
        3,
        10,
        3,
        &[2_000; SAMPLES_PER_FRAME],
    )
    .await;

    let mut buf = [0_u8; 512];
    let packet = tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let len = client_b.recv(&mut buf).await.unwrap();
            let packet = AudioPacket::decode(&buf[..len]).unwrap();
            let samples = pcm16_le_bytes_to_samples(&packet.payload).unwrap();
            if steady_level(&samples, 3_000) {
                return packet;
            }
        }
    })
    .await
    .unwrap();

    assert_eq!(packet.user_id, SERVER_USER_ID);
    assert_eq!(packet.target, AudioTarget::Mixed);

    server_task.abort();
}

#[tokio::test]
async fn consumes_queued_source_frames_in_order() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    send_control(
        control_addr,
        basic_config(1, Vec::new(), vec![10], TalkMode::Open),
    )
    .await;
    send_control(
        control_addr,
        basic_config(2, vec![10], Vec::new(), TalkMode::Muted),
    )
    .await;

    let client_a = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let client_b = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    send_packet(&client_b, server_addr, 2, 10, 1, &[0; SAMPLES_PER_FRAME]).await;
    send_packet(&client_a, server_addr, 1, 10, 2, &[100; SAMPLES_PER_FRAME]).await;
    send_packet(&client_a, server_addr, 1, 10, 3, &[200; SAMPLES_PER_FRAME]).await;

    let received = receive_distinct_sample_values(&client_b, 2).await;

    assert_eq!(received, vec![100, 200]);

    server_task.abort();
}

#[tokio::test]
async fn priority_talker_ducks_other_sources() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    send_control(
        control_addr,
        basic_config(1, Vec::new(), vec![10], TalkMode::Open),
    )
    .await;
    send_control(
        control_addr,
        basic_config(2, Vec::new(), vec![10], TalkMode::Open),
    )
    .await;
    send_control(
        control_addr,
        basic_config(3, vec![10], Vec::new(), TalkMode::Muted),
    )
    .await;
    send_control(
        control_addr,
        ControlMessage::Priority {
            user_id: 1,
            active: true,
        },
    )
    .await;

    let priority = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let normal = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    send_packet(&listener, server_addr, 3, 10, 1, &[0; SAMPLES_PER_FRAME]).await;
    send_packet(
        &priority,
        server_addr,
        1,
        10,
        2,
        &[1_000; SAMPLES_PER_FRAME],
    )
    .await;
    send_packet(&normal, server_addr, 2, 10, 3, &[1_000; SAMPLES_PER_FRAME]).await;

    let mut buf = [0_u8; 512];
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let len = listener.recv(&mut buf).await.unwrap();
            let packet = AudioPacket::decode(&buf[..len]).unwrap();
            let samples = pcm16_le_bytes_to_samples(&packet.payload).unwrap();
            if steady_level(&samples, 1_250) {
                return;
            }
        }
    })
    .await
    .unwrap();

    server_task.abort();
}

#[tokio::test]
async fn priority_talker_ducks_only_their_priority_channel() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    send_control(
        control_addr,
        ControlMessage::Config {
            user_id: 1,
            role: None,
            name: None,
            listen: Vec::new(),
            tx: vec![10],
            vol: HashMap::new(),
            talker_vol: None,
            codec: Some(Codec::Pcm16),
            opus_profile: None,
            talk_mode: Some(TalkMode::Open),
            priority: Some(true),
            priority_channels: Some(vec![10]),
            processing: None,
            buttons: None,
            ifb: None,
            stereo: None,
            esp32_audio: None,
        },
    )
    .await;
    send_control(
        control_addr,
        basic_config(2, Vec::new(), vec![11], TalkMode::Open),
    )
    .await;
    send_control(
        control_addr,
        basic_config(3, vec![10, 11], Vec::new(), TalkMode::Muted),
    )
    .await;

    let priority = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let normal = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    send_packet(&listener, server_addr, 3, 10, 1, &[0; SAMPLES_PER_FRAME]).await;
    send_packet(
        &priority,
        server_addr,
        1,
        10,
        2,
        &[1_000; SAMPLES_PER_FRAME],
    )
    .await;
    send_packet(&normal, server_addr, 2, 11, 3, &[1_000; SAMPLES_PER_FRAME]).await;

    receive_until_level(&listener, 2_000).await;
    server_task.abort();
}

#[tokio::test]
async fn ifb_ducks_program_only_while_interrupt_is_active() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    send_control(
        control_addr,
        ControlMessage::Config {
            user_id: 3,
            role: None,
            name: None,
            listen: vec![1, 2],
            tx: Vec::new(),
            vol: HashMap::new(),
            talker_vol: None,
            codec: Some(Codec::Pcm16),
            opus_profile: None,
            talk_mode: Some(TalkMode::Muted),
            priority: None,
            priority_channels: None,
            processing: None,
            buttons: None,
            ifb: Some(IfbConfig {
                enabled: true,
                program: vec![1],
                interrupt: vec![2],
                duck_gain: 0.125,
            }),
            stereo: None,
            esp32_audio: None,
        },
    )
    .await;
    send_control(
        control_addr,
        basic_config(1, Vec::new(), vec![1], TalkMode::Open),
    )
    .await;
    send_control(
        control_addr,
        basic_config(2, Vec::new(), vec![2], TalkMode::Open),
    )
    .await;

    let program = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let interrupt = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    send_packet(&listener, server_addr, 3, 1, 1, &[0; SAMPLES_PER_FRAME]).await;
    send_packet(&program, server_addr, 1, 1, 2, &[4_000; SAMPLES_PER_FRAME]).await;
    receive_until_level(&listener, 4_000).await;

    send_packet(&program, server_addr, 1, 1, 3, &[4_000; SAMPLES_PER_FRAME]).await;
    send_packet(
        &interrupt,
        server_addr,
        2,
        2,
        4,
        &[4_000; SAMPLES_PER_FRAME],
    )
    .await;
    receive_until_level(&listener, 4_500).await;

    server_task.abort();
}

#[tokio::test]
async fn per_talker_gain_applies_after_channel_gain() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    send_control(
        control_addr,
        ControlMessage::Config {
            user_id: 3,
            role: None,
            name: None,
            listen: vec![1],
            tx: Vec::new(),
            vol: [(1, 0.5)].into(),
            talker_vol: Some([(1, 0.25)].into()),
            codec: Some(Codec::Pcm16),
            opus_profile: None,
            talk_mode: Some(TalkMode::Muted),
            priority: None,
            priority_channels: None,
            processing: None,
            buttons: None,
            ifb: None,
            stereo: None,
            esp32_audio: None,
        },
    )
    .await;
    send_control(
        control_addr,
        basic_config(1, Vec::new(), vec![1], TalkMode::Open),
    )
    .await;

    let talker = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    send_packet(&listener, server_addr, 3, 1, 1, &[0; SAMPLES_PER_FRAME]).await;
    send_packet(&talker, server_addr, 1, 1, 2, &[8_000; SAMPLES_PER_FRAME]).await;
    receive_until_level(&listener, 1_000).await;

    server_task.abort();
}

#[tokio::test]
async fn ifb_ducking_does_not_inherit_priority_from_other_channels() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    send_control(
        control_addr,
        ControlMessage::Config {
            user_id: 3,
            role: None,
            name: None,
            listen: vec![1, 2],
            tx: Vec::new(),
            vol: HashMap::new(),
            talker_vol: None,
            codec: Some(Codec::Pcm16),
            opus_profile: None,
            talk_mode: Some(TalkMode::Muted),
            priority: None,
            priority_channels: None,
            processing: None,
            buttons: None,
            ifb: Some(IfbConfig {
                enabled: true,
                program: vec![1],
                interrupt: vec![2],
                duck_gain: 0.125,
            }),
            stereo: None,
            esp32_audio: None,
        },
    )
    .await;
    send_control(
        control_addr,
        basic_config(1, Vec::new(), vec![1], TalkMode::Open),
    )
    .await;
    send_control(
        control_addr,
        basic_config(2, Vec::new(), vec![2], TalkMode::Open),
    )
    .await;
    send_control(
        control_addr,
        ControlMessage::Priority {
            user_id: 2,
            active: true,
        },
    )
    .await;

    let program = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let interrupt = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    send_packet(&listener, server_addr, 3, 1, 1, &[0; SAMPLES_PER_FRAME]).await;
    send_packet(&program, server_addr, 1, 1, 2, &[4_000; SAMPLES_PER_FRAME]).await;
    send_packet(
        &interrupt,
        server_addr,
        2,
        2,
        3,
        &[4_000; SAMPLES_PER_FRAME],
    )
    .await;
    receive_until_level(&listener, 4_500).await;

    server_task.abort();
}

#[tokio::test]
async fn emergency_all_call_reaches_clients_outside_listen_config() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    send_control(
        control_addr,
        basic_config(1, Vec::new(), Vec::new(), TalkMode::Open),
    )
    .await;
    send_control(
        control_addr,
        basic_config(2, Vec::new(), Vec::new(), TalkMode::Muted),
    )
    .await;
    send_control(
        control_addr,
        ControlMessage::Emergency {
            user_id: 1,
            active: true,
            target: EmergencyTarget::All,
            duck_gain: 0.125,
            mute_others: false,
        },
    )
    .await;

    let source = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    send_registration_packet(&listener, server_addr, 2).await;
    send_target_packet(
        &source,
        server_addr,
        1,
        AudioTarget::Mixed,
        2,
        &[555; SAMPLES_PER_FRAME],
    )
    .await;

    receive_until_level(&listener, 555).await;
    server_task.abort();
}

#[tokio::test]
async fn emergency_mute_suppresses_normal_audio_for_recipients() {
    let server_socket = Arc::new(UdpSocket::bind("127.0.0.1:0").await.unwrap());
    let server_addr = server_socket.local_addr().unwrap();
    let control_listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let control_addr = control_listener.local_addr().unwrap();

    let server_task = tokio::spawn(server::run(Arc::clone(&server_socket), control_listener));
    send_control(
        control_addr,
        basic_config(1, Vec::new(), Vec::new(), TalkMode::Open),
    )
    .await;
    send_control(
        control_addr,
        basic_config(2, vec![10], Vec::new(), TalkMode::Muted),
    )
    .await;
    send_control(
        control_addr,
        basic_config(3, Vec::new(), vec![10], TalkMode::Open),
    )
    .await;
    send_control(
        control_addr,
        ControlMessage::Emergency {
            user_id: 1,
            active: true,
            target: EmergencyTarget::Users { users: vec![2] },
            duck_gain: 0.125,
            mute_others: true,
        },
    )
    .await;

    let emergency = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let normal = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let listener = UdpSocket::bind("127.0.0.1:0").await.unwrap();

    send_registration_packet(&listener, server_addr, 2).await;
    send_packet(&normal, server_addr, 3, 10, 1, &[4_000; SAMPLES_PER_FRAME]).await;
    send_target_packet(
        &emergency,
        server_addr,
        1,
        AudioTarget::Mixed,
        2,
        &[4_000; SAMPLES_PER_FRAME],
    )
    .await;

    receive_until_level(&listener, 4_000).await;
    server_task.abort();
}

async fn receive_distinct_sample_values(socket: &UdpSocket, count: usize) -> Vec<i16> {
    let mut values = Vec::new();
    let mut buf = [0_u8; 512];

    tokio::time::timeout(Duration::from_secs(2), async {
        while values.len() < count {
            let len = socket.recv(&mut buf).await.unwrap();
            let packet = AudioPacket::decode(&buf[..len]).unwrap();
            let samples = pcm16_le_bytes_to_samples(&packet.payload).unwrap();
            let value = steady_sample_value(&samples);
            if values.last() != Some(&value) {
                values.push(value);
            }
        }
    })
    .await
    .unwrap();

    values
}

async fn receive_until_level(socket: &UdpSocket, expected: i16) {
    let mut buf = [0_u8; 512];
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let len = socket.recv(&mut buf).await.unwrap();
            let packet = AudioPacket::decode(&buf[..len]).unwrap();
            let samples = pcm16_le_bytes_to_samples(&packet.payload).unwrap();
            if steady_level(&samples, expected) {
                return;
            }
        }
    })
    .await
    .unwrap();
}

fn steady_sample_value(samples: &[i16]) -> i16 {
    samples[samples.len() / 2]
}

fn steady_level(samples: &[i16], expected: i16) -> bool {
    samples
        .iter()
        .skip(8)
        .all(|sample| (*sample - expected).abs() <= 32)
}

fn assert_steady_level(samples: &[i16], expected: i16) {
    assert!(
        steady_level(samples, expected),
        "expected steady level around {expected}, got {samples:?}"
    );
}

fn basic_config(
    user_id: u16,
    listen: Vec<u16>,
    tx: Vec<u16>,
    talk_mode: TalkMode,
) -> ControlMessage {
    config_with_codec(user_id, listen, tx, talk_mode, Codec::Pcm16)
}

fn config_with_codec(
    user_id: u16,
    listen: Vec<u16>,
    tx: Vec<u16>,
    talk_mode: TalkMode,
    codec: Codec,
) -> ControlMessage {
    ControlMessage::Config {
        user_id,
        role: None,
        name: None,
        listen,
        tx,
        vol: HashMap::new(),
        talker_vol: None,
        codec: Some(codec),
        opus_profile: None,
        talk_mode: Some(talk_mode),
        priority: None,
        priority_channels: None,
        processing: None,
        buttons: None,
        ifb: None,
        stereo: None,
        esp32_audio: None,
    }
}

async fn send_packet(
    socket: &UdpSocket,
    server_addr: std::net::SocketAddr,
    user_id: u16,
    channel_id: u16,
    seq: u16,
    samples: &[i16],
) {
    send_target_packet(
        socket,
        server_addr,
        user_id,
        AudioTarget::Channel(channel_id),
        seq,
        samples,
    )
    .await;
}

async fn send_target_packet(
    socket: &UdpSocket,
    server_addr: std::net::SocketAddr,
    user_id: u16,
    target: AudioTarget,
    seq: u16,
    samples: &[i16],
) {
    let packet = AudioPacket {
        user_id,
        target,
        codec: Codec::Pcm16,
        seq,
        timestamp: seq as u32 * 160,
        payload: pcm16_samples_to_le_bytes(samples),
    };
    let mut encoded = Vec::new();
    packet.encode(&mut encoded).unwrap();
    socket.send_to(&encoded, server_addr).await.unwrap();
}

async fn send_registration_packet(
    socket: &UdpSocket,
    server_addr: std::net::SocketAddr,
    user_id: u16,
) {
    send_registration_packet_with_codec(socket, server_addr, user_id, Codec::Pcm16).await;
}

async fn send_registration_packet_with_codec(
    socket: &UdpSocket,
    server_addr: std::net::SocketAddr,
    user_id: u16,
    codec: Codec,
) {
    let packet = AudioPacket::registration(user_id, codec, 0);
    let mut encoded = Vec::new();
    packet.encode(&mut encoded).unwrap();
    socket.send_to(&encoded, server_addr).await.unwrap();
}

async fn send_control(control_addr: std::net::SocketAddr, message: ControlMessage) {
    let (mut ws, _) = tokio_tungstenite::connect_async(format!("ws://{control_addr}"))
        .await
        .unwrap();
    ws.send(Message::Text(serde_json::to_string(&message).unwrap()))
        .await
        .unwrap();
    let response = ws.next().await.unwrap().unwrap();
    let Message::Text(text) = response else {
        panic!("expected text control response");
    };
    let response = serde_json::from_str::<ControlResponse>(&text).unwrap();
    assert!(matches!(response, ControlResponse::Ack), "{response:?}");
}
