//! The Continuity Capture session over Rapport, as the host sends it
//! (`docs/guest-pairing.md`, "Phase B"; from tvOS 27.2's
//! CMContinuityCaptureHost, `setupRPDisplaySessionForConfiguration:` and the
//! blocks after it).
//!
//! Every key is a string equal to its own symbol name.

use apple_opack::Value;

use crate::archive::{Archive, Field};

/// The RemoteDisplay session's event, both ways.
pub const SESSION_EVENT: &str = "ContinuityCaptureSessionEventID";
/// The event on the Control, Command and Data streams.
pub const STREAM_EVENT: &str = "ContinuityCaptureStreamEventID";

pub const MESSAGE_TYPE: &str = "ContinuityCaptureRapportClientMessageTypeKey";
pub const SESSION_ID: &str = "ContinuityCaptureRapportClientSessionIDKey";
pub const PRE_START_CONFIGURATION: &str = "ContinuityCaptureRapportClientPreStartConfigurationKey";
pub const STREAMS_SETUP: &str = "ContinuityCaptureRapportClientStreamsSetupKey";
pub const STREAM_IDENTIFIER: &str = "ContinuityCaptureRapportClientSetStreamMessageDataIdentifierKey";
pub const STREAM_IS_MEDIA: &str = "ContinuityCaptureRapportClientSetStreamMessageDataIsMediaTypeKey";
pub const EVENT_NAME: &str = "ContinuityCaptureRapportClientEventNameKey";

/// `MessageType` values of the session event.
pub mod message {
    /// Host → iPhone: the session starts (configuration, streams).
    pub const SESSION_START: u64 = 0;
    /// iPhone → host: it has started.
    pub const SESSION_STARTED: u64 = 1;
    /// Host → iPhone: a stream exists; in the setup, and again after 1.
    pub const STREAM_ANNOUNCE: u64 = 2;
    /// Host → iPhone: a stream's connection is up.
    pub const STREAM_ACTIVATED: u64 = 3;
    /// iPhone → host: termination complete.
    pub const TERMINATE_COMPLETE: u64 = 5;
    /// iPhone → host: an event (`EVENT_NAME`).
    pub const POST_EVENT: u64 = 6;
}

/// The session's streams: three Rapport connections for messages, and the
/// media: the microphone's, or the camera's.
pub mod stream {
    pub const CONTROL: &str = "ContinuityCaptureControl";
    pub const COMMAND: &str = "ContinuityCaptureCommand";
    pub const DATA: &str = "ContinuityCaptureData";
    pub const MICROPHONE: &str = "ContinuityCaptureMediaMicrophone";
    pub const DEFAULT_VIDEO: &str = "ContinuityCaptureMediaIdentifierDefaultVideo";
}

/// The keys of a stream message (`STREAM_EVENT`): a selector and its
/// arguments.
pub const SELECTOR: &str = "ContinuityCaptureSelector";
pub const ARGS: &str = "ContinuityCaptureArgs";

/// Selectors on the Command stream.
pub mod command {
    pub const START_STREAM: u64 = 1;
    pub const STOP_STREAM: u64 = 2;
    pub const POST_EVENT: u64 = 3;
    pub const SYNCHRONIZE_AUDIO_CLOCK: u64 = 4;
    pub const SYNCHRONIZE_AUDIO_CLOCK_COMPLETE: u64 = 5;
}

/// The iPhone's events (Command, `POST_EVENT`, `[name, entity]`), sent for
/// every entity it has.
pub mod event {
    /// Its user's Disconnect: the session's end.
    pub const USER_DISCONNECT: &str = "kCMContinuityCaptureEventUserDisconnect";
    /// Its own end of the session when a stream fails, as a media stream does
    /// 12 s without RTCP (`docs/guest-pairing.md`, trials 17 and 27).
    pub const CONNECTION_DISCONNECT: &str = "kCMContinuityCaptureEventConnectionDisconnect";
    /// Its user's Pause.
    pub const USER_PAUSE: &str = "kCMContinuityCaptureEventUserPause";
    /// Its user's Resume, which also comes as every session starts.
    pub const USER_RESUME: &str = "kCMContinuityCaptureEventUserResume";
}

/// Selectors on the Control stream.
pub mod control {
    pub const SET_VALUE: u64 = 1;
    /// The iPhone's AVConference offer (`[entity, blob]`), and the host's
    /// answer.
    pub const AVC_NEGOTIATION: u64 = 2;
}

/// The host's StartStream command, on the Command stream: the archived
/// configuration, an option, and 0.
pub fn start_stream(configuration: &Configuration, option: u64) -> Value {
    dict(vec![
        (SELECTOR, Value::int(command::START_STREAM)),
        (ARGS, Value::Array(vec![Value::Bytes(configuration.archive()), Value::int(option), Value::int(0)])),
    ])
}

/// The iPhone's capture devices (`ContinuityCaptureEntityType`): 0 is all
/// of them, 2 the Desk View camera, 4 its system notifications.
pub const ENTITY_DEFAULT_VIDEO: i64 = 1;
pub const ENTITY_MICROPHONE: i64 = 3;
/// `audioDeviceMode` 'call': the iPhone then offers 4-channel audio.
pub const AUDIO_MODE_CALL: i32 = i32::from_be_bytes(*b"call");
/// `'420v'`, the pixel format of every camera format the iPhone offers.
pub const PIXEL_FORMAT_420V: u32 = u32::from_be_bytes(*b"420v");

/// A `CMContinuityCaptureStreamFormat`: one of the camera formats the iPhone
/// offers a host. iOS 27's are fixed
/// (`+[CMContinuityCaptureDeviceCapabilities
/// _resolvedStreamFormatsForEntityType:requireManualFramingSupport:]`):
/// 1920×1440, 1920×1080, 1280×720 and 640×480, '420v', 1 to 30 fps (or to
/// 60, named with "p60"), entity 1, version 1.0.0, kept if the camera has
/// them. The iPhone matches a host's choice to its camera on the size, the
/// pixel format and the top frame rate only.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamFormat {
    pub name: String,
    pub entity: i64,
    pub width: i64,
    pub height: i64,
    pub pixel_format: u32,
    pub min_frame_rate: i64,
    pub max_frame_rate: i64,
    /// The least Continuity Capture version that takes it: (major, minor,
    /// patch).
    pub minimum_version: (i64, i64, i64),
}

impl StreamFormat {
    /// 1920×1080 at up to 30 fps, which every camera the iPhone lists has.
    pub fn hd_1080p30() -> Self {
        Self {
            name: "1920x1080".into(),
            entity: ENTITY_DEFAULT_VIDEO,
            width: 1920,
            height: 1080,
            pixel_format: PIXEL_FORMAT_420V,
            min_frame_rate: 1,
            max_frame_rate: 30,
            minimum_version: (1, 0, 0),
        }
    }

    /// Its archived form (`-[CMContinuityCaptureStreamFormat encodeWithCoder:]`).
    fn archive_into(&self, archive: &mut Archive) -> plist::Uid {
        let name = archive.string(&self.name);
        archive.object(
            &["CMContinuityCaptureStreamFormat", "NSObject"],
            vec![
                ("name", Field::Object(name)),
                ("entity", Field::Int(self.entity)),
                ("width", Field::Int(self.width)),
                ("height", Field::Int(self.height)),
                ("pixelFormat", Field::Int(i64::from(self.pixel_format))),
                ("minFrameRate", Field::Int(self.min_frame_rate)),
                ("maxFrameRate", Field::Int(self.max_frame_rate)),
                ("minimumSupportedVersionMajor", Field::Int(self.minimum_version.0)),
                ("minimumSupportedVersionMinor", Field::Int(self.minimum_version.1)),
                ("minimumSupportedVersionPatch", Field::Int(self.minimum_version.2)),
            ],
        )
    }
}

/// The part of a `CMContinuityCaptureConfiguration` Koala sets: the
/// microphone's, or the camera's. Every effect (Center Stage, Portrait,
/// Studio Light and the rest) is left out, which the iPhone reads as off.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Configuration {
    pub entity: i64,
    /// A fresh UUID per activation. Its first 14 bytes salt the SRTP key.
    pub session_id: [u8; 16],
    pub generation_id: i64,
    pub audio_device_mode: i32,
    /// The camera's format, for the camera.
    pub format: Option<StreamFormat>,
    pub frame_rate: i64,
    pub min_frame_rate: i64,
}

impl Configuration {
    pub fn microphone(session_id: [u8; 16], generation_id: i64) -> Self {
        Self {
            entity: ENTITY_MICROPHONE,
            session_id,
            generation_id,
            audio_device_mode: AUDIO_MODE_CALL,
            format: None,
            frame_rate: 0,
            min_frame_rate: 0,
        }
    }

    /// The camera (entity 1), in `format`, at its top frame rate.
    pub fn camera(session_id: [u8; 16], generation_id: i64, format: StreamFormat) -> Self {
        Self {
            entity: ENTITY_DEFAULT_VIDEO,
            session_id,
            generation_id,
            audio_device_mode: 0,
            frame_rate: format.max_frame_rate,
            min_frame_rate: format.min_frame_rate,
            format: Some(format),
        }
    }

    /// The media stream it uses.
    pub fn media_stream(&self) -> &'static str {
        if self.entity == ENTITY_DEFAULT_VIDEO { stream::DEFAULT_VIDEO } else { stream::MICROPHONE }
    }

    /// The NSKeyedArchiver archive (`requiringSecureCoding:YES`) the
    /// iPhone unarchives as `CMContinuityCaptureConfiguration`.
    pub fn archive(&self) -> Vec<u8> {
        let mut archive = Archive::new();
        let session_id = archive.uuid(self.session_id);
        let format = match &self.format {
            Some(format) => format.archive_into(&mut archive),
            None => Archive::null(),
        };
        let root = archive.object(
            &["CMContinuityCaptureConfiguration", "NSObject"],
            vec![
                ("entity", Field::Int(self.entity)),
                ("generationID", Field::Int(self.generation_id)),
                ("sessionID", Field::Object(session_id)),
                ("audioDeviceMode", Field::Int(i64::from(self.audio_device_mode))),
                ("format", Field::Object(format)),
                ("frameRate", Field::Int(self.frame_rate)),
                ("minFrameRate", Field::Int(self.min_frame_rate)),
            ],
        );
        archive.finish(root)
    }
}

/// Every stream a session of `configurations` uses: Control, Command and
/// Data, then each entity's media stream (`docs/guest-pairing.md`, "Both at
/// once").
pub fn streams(configurations: &[Configuration]) -> Vec<(&'static str, bool)> {
    [(stream::CONTROL, false), (stream::COMMAND, false), (stream::DATA, false)]
        .into_iter()
        .chain(configurations.iter().map(|c| (c.media_stream(), true)))
        .collect()
}

/// The session-start event's content (`MessageType` 0): the session's
/// number, the first configuration archived (the iPhone takes one, and
/// starts it early), and every stream the session will use. One session
/// runs every entity: the iPhone keeps a device per entity, and streams
/// them side by side.
pub fn session_start(session: u64, configurations: &[Configuration]) -> Value {
    let streams =
        streams(configurations).into_iter().map(|(name, media)| stream_announce(name, media)).collect::<Vec<_>>();
    let mut content = vec![(MESSAGE_TYPE, Value::int(message::SESSION_START)), (SESSION_ID, Value::int(session))];
    if let Some(first) = configurations.first() {
        content.push((PRE_START_CONFIGURATION, Value::Bytes(first.archive())));
    }
    content.push((STREAMS_SETUP, Value::Array(streams)));
    dict(content)
}

/// A stream's announcement (`MessageType` 2).
pub fn stream_announce(name: &str, media: bool) -> Value {
    dict(vec![
        (MESSAGE_TYPE, Value::int(message::STREAM_ANNOUNCE)),
        (STREAM_IDENTIFIER, name.into()),
        (STREAM_IS_MEDIA, media.into()),
    ])
}

/// A stream's activation (`MessageType` 3).
pub fn stream_activated(name: &str) -> Value {
    dict(vec![(MESSAGE_TYPE, Value::int(message::STREAM_ACTIVATED)), (STREAM_IDENTIFIER, name.into())])
}

fn dict(pairs: Vec<(&str, Value)>) -> Value {
    Value::Dict(pairs.into_iter().map(|(k, v)| (Value::from(k), v)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_configuration_archives_as_its_class() {
        let configuration = Configuration::microphone([1; 16], 7);
        let bytes = configuration.archive();
        let value = plist::Value::from_reader(std::io::Cursor::new(&bytes)).unwrap();
        let objects = value.as_dictionary().unwrap().get("$objects").and_then(plist::Value::as_array).unwrap();
        let root = objects.iter().find_map(|o| o.as_dictionary().filter(|d| d.contains_key("entity"))).unwrap();
        assert_eq!(root.get("entity").and_then(plist::Value::as_signed_integer), Some(3));
        assert_eq!(root.get("audioDeviceMode").and_then(plist::Value::as_signed_integer), Some(0x6361_6c6c));
        let class = &objects[root.get("$class").unwrap().as_uid().unwrap().get() as usize];
        let name = class.as_dictionary().unwrap().get("$classname").and_then(plist::Value::as_string);
        assert_eq!(name, Some("CMContinuityCaptureConfiguration"));
    }

    #[test]
    fn the_camera_configuration_carries_its_format() {
        let configuration = Configuration::camera([3; 16], 1, StreamFormat::hd_1080p30());
        let bytes = configuration.archive();
        let value = plist::Value::from_reader(std::io::Cursor::new(&bytes)).unwrap();
        let objects = value.as_dictionary().unwrap().get("$objects").and_then(plist::Value::as_array).unwrap();
        let root = objects.iter().find_map(|o| o.as_dictionary().filter(|d| d.contains_key("sessionID"))).unwrap();
        assert_eq!(root.get("entity").and_then(plist::Value::as_signed_integer), Some(1));
        assert_eq!(root.get("frameRate").and_then(plist::Value::as_signed_integer), Some(30));
        let format = &objects[root.get("format").unwrap().as_uid().unwrap().get() as usize];
        let format = format.as_dictionary().unwrap();
        let int = |key: &str| format.get(key).and_then(plist::Value::as_signed_integer);
        assert_eq!((int("width"), int("height"), int("maxFrameRate")), (Some(1920), Some(1080), Some(30)));
        assert_eq!(int("pixelFormat"), Some(0x3432_3076), "'420v'");
        let name = &objects[format.get("name").unwrap().as_uid().unwrap().get() as usize];
        assert_eq!(name.as_string(), Some("1920x1080"));
        let class = &objects[format.get("$class").unwrap().as_uid().unwrap().get() as usize];
        let class = class.as_dictionary().unwrap().get("$classname").and_then(plist::Value::as_string);
        assert_eq!(class, Some("CMContinuityCaptureStreamFormat"));

        let start = session_start(1, std::slice::from_ref(&configuration));
        let Some(Value::Array(streams)) = start.get(STREAMS_SETUP) else { panic!("no streams") };
        let media: Vec<&str> = streams
            .iter()
            .filter(|s| s.get(STREAM_IS_MEDIA) == Some(&Value::Bool(true)))
            .filter_map(|s| s.get(STREAM_IDENTIFIER).and_then(Value::as_str))
            .collect();
        assert_eq!(media, [stream::DEFAULT_VIDEO]);
    }

    #[test]
    fn the_session_start_lists_four_streams_one_media() {
        let start = session_start(1, &[Configuration::microphone([2; 16], 1)]);
        assert_eq!(start.get(MESSAGE_TYPE).and_then(Value::as_u64), Some(0));
        let Some(Value::Array(streams)) = start.get(STREAMS_SETUP) else { panic!("no streams") };
        assert_eq!(streams.len(), 4);
        let media: Vec<&str> = streams
            .iter()
            .filter(|s| s.get(STREAM_IS_MEDIA) == Some(&Value::Bool(true)))
            .filter_map(|s| s.get(STREAM_IDENTIFIER).and_then(Value::as_str))
            .collect();
        assert_eq!(media, [stream::MICROPHONE]);
        assert!(apple_opack::pack(&start).is_ok());
    }

    #[test]
    fn both_entities_share_one_session_with_a_media_stream_each() {
        let microphone = Configuration::microphone([4; 16], 1);
        let camera = Configuration::camera([5; 16], 1, StreamFormat::hd_1080p30());
        let start = session_start(1, &[microphone.clone(), camera]);
        let Some(Value::Array(streams)) = start.get(STREAMS_SETUP) else { panic!("no streams") };
        assert_eq!(streams.len(), 5);
        let media: Vec<&str> = streams
            .iter()
            .filter(|s| s.get(STREAM_IS_MEDIA) == Some(&Value::Bool(true)))
            .filter_map(|s| s.get(STREAM_IDENTIFIER).and_then(Value::as_str))
            .collect();
        assert_eq!(media, [stream::MICROPHONE, stream::DEFAULT_VIDEO]);
        // The first configuration is the one started early.
        assert_eq!(start.get(PRE_START_CONFIGURATION), Some(&Value::Bytes(microphone.archive())));
        assert!(apple_opack::pack(&start).is_ok());
    }
}
