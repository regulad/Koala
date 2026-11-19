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
/// microphone's media.
pub mod stream {
    pub const CONTROL: &str = "ContinuityCaptureControl";
    pub const COMMAND: &str = "ContinuityCaptureCommand";
    pub const DATA: &str = "ContinuityCaptureData";
    pub const MICROPHONE: &str = "ContinuityCaptureMediaMicrophone";
}

/// The microphone's entity.
pub const ENTITY_MICROPHONE: i64 = 3;
/// `audioDeviceMode` 'call': the iPhone then offers 4-channel audio.
pub const AUDIO_MODE_CALL: i32 = i32::from_be_bytes(*b"call");

/// The part of a `CMContinuityCaptureConfiguration` a mic-only host sets.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Configuration {
    pub entity: i64,
    /// A fresh UUID per activation. Its first 14 bytes salt the SRTP key.
    pub session_id: [u8; 16],
    pub generation_id: i64,
    pub audio_device_mode: i32,
}

impl Configuration {
    pub fn microphone(session_id: [u8; 16], generation_id: i64) -> Self {
        Self { entity: ENTITY_MICROPHONE, session_id, generation_id, audio_device_mode: AUDIO_MODE_CALL }
    }

    /// The NSKeyedArchiver archive (`requiringSecureCoding:YES`) the
    /// iPhone unarchives as `CMContinuityCaptureConfiguration`.
    pub fn archive(&self) -> Vec<u8> {
        let mut archive = Archive::new();
        let session_id = archive.uuid(self.session_id);
        let root = archive.object(
            &["CMContinuityCaptureConfiguration", "NSObject"],
            vec![
                ("entity", Field::Int(self.entity)),
                ("generationID", Field::Int(self.generation_id)),
                ("sessionID", Field::Object(session_id)),
                ("audioDeviceMode", Field::Int(i64::from(self.audio_device_mode))),
                ("format", Field::Object(Archive::null())),
                ("frameRate", Field::Int(0)),
                ("minFrameRate", Field::Int(0)),
            ],
        );
        archive.finish(root)
    }
}

/// The session-start event's content (`MessageType` 0): the session's
/// number, its archived configuration, and every stream it will use.
pub fn session_start(session: u64, configuration: &Configuration) -> Value {
    let streams =
        [(stream::CONTROL, false), (stream::COMMAND, false), (stream::DATA, false), (stream::MICROPHONE, true)]
            .into_iter()
            .map(|(name, media)| stream_announce(name, media))
            .collect::<Vec<_>>();
    dict(vec![
        (MESSAGE_TYPE, Value::int(message::SESSION_START)),
        (SESSION_ID, Value::int(session)),
        (PRE_START_CONFIGURATION, Value::Bytes(configuration.archive())),
        (STREAMS_SETUP, Value::Array(streams)),
    ])
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
    fn the_session_start_lists_four_streams_one_media() {
        let start = session_start(1, &Configuration::microphone([2; 16], 1));
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
}
