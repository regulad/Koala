//! Rapport's OPACK messages: requests, responses and events.
//!
//! Each is an OPACK dictionary with `_t` (1 event, 2 request, 3 response),
//! `_i` (the request's or event's name), `_x` (a transaction number, which a
//! response repeats) and `_c` (the content, a dictionary).

use apple_opack::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MessageType {
    Event = 1,
    Request = 2,
    Response = 3,
}

/// A decoded message.
#[derive(Clone, Debug, PartialEq)]
pub struct Message {
    pub kind: MessageType,
    /// `_i`: the request's or event's name.
    pub id: Option<String>,
    /// `_x`: the transaction number.
    pub xid: Option<u64>,
    /// `_c`: the content.
    pub content: Value,
    /// The whole dictionary, for keys this type doesn't name (such as an
    /// error's).
    pub raw: Value,
}

impl Message {
    pub fn request(id: &str, xid: u64, content: Value) -> Value {
        dict(vec![("_i", id.into()), ("_x", Value::int(xid)), ("_t", Value::int(2)), ("_c", content)])
    }

    pub fn response(xid: u64, content: Value) -> Value {
        dict(vec![("_x", Value::int(xid)), ("_t", Value::int(3)), ("_c", content)])
    }

    pub fn event(id: &str, xid: u64, content: Value) -> Value {
        dict(vec![("_i", id.into()), ("_x", Value::int(xid)), ("_t", Value::int(1)), ("_c", content)])
    }

    /// Reads a message, or `None` if `value` isn't one.
    pub fn parse(value: Value) -> Option<Self> {
        let kind = match value.get("_t")?.as_u64()? {
            1 => MessageType::Event,
            2 => MessageType::Request,
            3 => MessageType::Response,
            _ => return None,
        };
        Some(Self {
            kind,
            id: value.get("_i").and_then(Value::as_str).map(str::to_owned),
            xid: value.get("_x").and_then(Value::as_u64),
            content: value.get("_c").cloned().unwrap_or(Value::Dict(Vec::new())),
            raw: value,
        })
    }
}

/// An OPACK dictionary with string keys, in order.
pub fn dict(pairs: Vec<(&str, Value)>) -> Value {
    Value::Dict(pairs.into_iter().map(|(k, v)| (Value::from(k), v)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_request_survives_encoding() {
        let request = Message::request("_systemInfo", 7, dict(vec![("name", "Koala".into())]));
        let bytes = apple_opack::pack(&request).unwrap();
        let (value, rest) = apple_opack::unpack(&bytes).unwrap();
        assert!(rest.is_empty());
        let message = Message::parse(value).unwrap();
        assert_eq!(message.kind, MessageType::Request);
        assert_eq!(message.id.as_deref(), Some("_systemInfo"));
        assert_eq!(message.xid, Some(7));
        assert_eq!(message.content.get("name").and_then(Value::as_str), Some("Koala"));
    }
}
