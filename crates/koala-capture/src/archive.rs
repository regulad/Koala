//! NSKeyedArchiver archives, written: the format Foundation's
//! `NSKeyedArchiver` produces and `NSKeyedUnarchiver` reads, as a binary
//! property list.
//!
//! An archive is a dictionary: `$archiver` = "NSKeyedArchiver", `$version`
//! = 100000, `$top` = {"root": UID}, and `$objects`, a flat list whose first
//! entry is the string "$null" (UID 0, nil). Each object is a dictionary of
//! its encoded keys plus `$class`, a UID of a class entry `{$classname,
//! $classes: [the class, its superclasses…]}`. Integers, booleans and reals
//! are stored inline; objects as UIDs.

use plist::{Dictionary, Uid, Value};

/// An archive being built.
pub struct Archive {
    objects: Vec<Value>,
}

/// A field of an encoded object.
pub enum Field {
    Int(i64),
    Bool(bool),
    Real(f64),
    /// Another object in the archive; [`Archive::null`] is nil.
    Object(Uid),
}

impl Archive {
    /// nil.
    pub fn null() -> Uid {
        Uid::new(0)
    }

    pub fn new() -> Self {
        Self { objects: vec![Value::String("$null".into())] }
    }

    /// Adds an object of class `classes[0]` (whose superclasses follow,
    /// ending with `NSObject`) with `fields`.
    pub fn object(&mut self, classes: &[&str], fields: Vec<(&str, Field)>) -> Uid {
        let mut dict = Dictionary::new();
        // The object first, then its class: the order Foundation writes.
        let at = self.push(Value::Boolean(false));
        for (key, field) in fields {
            let value = match field {
                Field::Int(n) => Value::Integer(n.into()),
                Field::Bool(b) => Value::Boolean(b),
                Field::Real(r) => Value::Real(r),
                Field::Object(uid) => Value::Uid(uid),
            };
            dict.insert(key.into(), value);
        }
        let class = self.class(classes);
        dict.insert("$class".into(), Value::Uid(class));
        self.objects[at.get() as usize] = Value::Dictionary(dict);
        at
    }

    /// Adds an `NSUUID`.
    pub fn uuid(&mut self, bytes: [u8; 16]) -> Uid {
        let at = self.push(Value::Boolean(false));
        let class = self.class(&["NSUUID", "NSObject"]);
        let mut dict = Dictionary::new();
        dict.insert("NS.uuidbytes".into(), Value::Data(bytes.to_vec()));
        dict.insert("$class".into(), Value::Uid(class));
        self.objects[at.get() as usize] = Value::Dictionary(dict);
        at
    }

    /// Adds a string.
    pub fn string(&mut self, s: &str) -> Uid {
        self.push(Value::String(s.into()))
    }

    /// The archive, with `root` at its top, as a binary property list.
    pub fn finish(self, root: Uid) -> Vec<u8> {
        let mut top = Dictionary::new();
        top.insert("root".into(), Value::Uid(root));
        let mut archive = Dictionary::new();
        archive.insert("$version".into(), Value::Integer(100_000.into()));
        archive.insert("$archiver".into(), Value::String("NSKeyedArchiver".into()));
        archive.insert("$top".into(), Value::Dictionary(top));
        archive.insert("$objects".into(), Value::Array(self.objects));
        let mut out = Vec::new();
        Value::Dictionary(archive).to_writer_binary(&mut out).expect("writing to a Vec doesn't fail");
        out
    }

    /// A class entry, shared between objects of the same class.
    fn class(&mut self, classes: &[&str]) -> Uid {
        let found = self.objects.iter().position(|o| {
            o.as_dictionary().and_then(|d| d.get("$classname")).and_then(Value::as_string) == Some(classes[0])
        });
        if let Some(at) = found {
            return Uid::new(at as u64);
        }
        let mut dict = Dictionary::new();
        dict.insert("$classname".into(), Value::String(classes[0].into()));
        dict.insert("$classes".into(), Value::Array(classes.iter().map(|c| Value::String((*c).into())).collect()));
        self.push(Value::Dictionary(dict))
    }

    fn push(&mut self, value: Value) -> Uid {
        self.objects.push(value);
        Uid::new(self.objects.len() as u64 - 1)
    }
}

impl Default for Archive {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_object_with_a_uuid_reads_back() {
        let mut archive = Archive::new();
        let id = archive.uuid([7; 16]);
        let root = archive.object(
            &["Thing", "NSObject"],
            vec![("count", Field::Int(3)), ("on", Field::Bool(true)), ("id", Field::Object(id))],
        );
        let bytes = archive.finish(root);
        assert!(bytes.starts_with(b"bplist00"));

        let value = Value::from_reader(std::io::Cursor::new(&bytes)).unwrap();
        let archive = value.as_dictionary().unwrap();
        assert_eq!(archive.get("$archiver").and_then(Value::as_string), Some("NSKeyedArchiver"));
        let objects = archive.get("$objects").and_then(Value::as_array).unwrap();
        assert_eq!(objects[0].as_string(), Some("$null"));
        let root = archive.get("$top").and_then(Value::as_dictionary).and_then(|t| t.get("root")).unwrap();
        let root = objects[root.as_uid().unwrap().get() as usize].as_dictionary().unwrap();
        assert_eq!(root.get("count").and_then(Value::as_signed_integer), Some(3));
        assert_eq!(root.get("on").and_then(Value::as_boolean), Some(true));
        let class = &objects[root.get("$class").unwrap().as_uid().unwrap().get() as usize];
        assert_eq!(class.as_dictionary().unwrap().get("$classname").and_then(Value::as_string), Some("Thing"));
        let uuid = &objects[root.get("id").unwrap().as_uid().unwrap().get() as usize];
        assert_eq!(uuid.as_dictionary().unwrap().get("NS.uuidbytes").and_then(Value::as_data), Some(&[7u8; 16][..]));
    }
}
