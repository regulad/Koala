//! Which microphone each of the iPhone's audio channels is.
//!
//! The map is `data/mic-maps.json`, which `tools/mic_maps.py` generates from
//! the device trees of every iPhone an iOS build supports: each names its
//! built-in microphones by code (`imic`, `smic`, `fmic`, `lmic`), and the
//! channels come in codec-input order. What each code means came from
//! listening to one model's channels; the file says which.
//!
//! The model is the one in the iPhone's AVC offer (its call info,
//! [`crate::avc::call_info::DEVICE_TYPE`]); the channel count is the
//! stream's. A model the map doesn't list gets the most common order, and a
//! stream whose channel count doesn't match gets plain numbered microphones.

use std::collections::BTreeMap;
use std::fmt;
use std::sync::OnceLock;

use serde::Deserialize;

const MAPS: &str = include_str!("../data/mic-maps.json");

#[derive(Debug, Deserialize)]
struct Maps {
    /// The iOS build the device trees came from.
    build: String,
    mics: BTreeMap<String, Meaning>,
    default: Vec<String>,
    models: BTreeMap<String, Model>,
}

#[derive(Debug, Deserialize)]
struct Meaning {
    id: String,
    name: String,
    place: String,
}

#[derive(Debug, Deserialize)]
struct Model {
    name: String,
    channels: Vec<String>,
    /// Codes that trade channels on a variant board.
    #[serde(default)]
    swappable: Vec<String>,
    /// How the order was checked against this model's audio, if it was.
    checked: Option<String>,
}

fn maps() -> &'static Maps {
    static MAPS_PARSED: OnceLock<Maps> = OnceLock::new();
    MAPS_PARSED.get_or_init(|| serde_json::from_str(MAPS).expect("data/mic-maps.json parses (tested)"))
}

/// The iOS build the map was generated from.
pub fn build() -> &'static str {
    &maps().build
}

/// One of the iPhone's microphones: a channel of its stream.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mic {
    pub channel: usize,
    /// The device tree's code, when the map names the channel.
    pub code: Option<&'static str>,
    /// For file names and device IDs: `bottom-1`, `back`, … or `mic-1`, ….
    pub id: String,
    /// For people: `Bottom 1`, `Back`, … or `Mic 1`, ….
    pub name: String,
    /// Where it is on the phone, when known.
    pub place: Option<&'static str>,
}

/// How a stream's channels were named.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    /// From the model's own entry.
    Model,
    /// The model isn't listed (or wasn't known): the most common order.
    Default,
    /// The channel count doesn't match the map: numbered only.
    Numbered,
}

/// The microphones of one stream.
#[derive(Clone, Debug)]
pub struct Layout {
    /// The iPhone's model identifier, such as `iPhone18,1`.
    pub model: Option<String>,
    /// Its marketing name, if the map lists it.
    pub product: Option<&'static str>,
    pub source: Source,
    /// Whether the order was checked against this model's audio.
    pub checked: bool,
    /// Pairs of channels that may be the other way round on this model.
    pub swappable: Vec<(usize, usize)>,
    /// One per channel, in channel order.
    pub mics: Vec<Mic>,
}

/// The microphones of a stream of `channels` channels from `model`.
pub fn layout(model: Option<&str>, channels: usize) -> Layout {
    let maps = maps();
    let entry = model.and_then(|m| maps.models.get(m));
    let order = entry.map_or(&maps.default, |e| &e.channels);
    let mut layout = Layout {
        model: model.map(str::to_owned),
        product: entry.map(|e| e.name.as_str()),
        source: if entry.is_some() { Source::Model } else { Source::Default },
        checked: entry.is_some_and(|e| e.checked.is_some()),
        swappable: Vec::new(),
        mics: Vec::new(),
    };
    if order.len() != channels {
        layout.source = Source::Numbered;
        layout.mics = (0..channels)
            .map(|c| Mic {
                channel: c,
                code: None,
                id: format!("mic-{}", c + 1),
                name: format!("Mic {}", c + 1),
                place: None,
            })
            .collect();
        return layout;
    }
    layout.mics = order
        .iter()
        .enumerate()
        .map(|(channel, code)| {
            let meaning = &maps.mics[code];
            Mic {
                channel,
                code: Some(code.as_str()),
                id: meaning.id.clone(),
                name: meaning.name.clone(),
                place: Some(meaning.place.as_str()),
            }
        })
        .collect();
    if let Some(e) = entry {
        let at = |code: &String| order.iter().position(|c| c == code);
        if let [a, b] = e.swappable.as_slice()
            && let (Some(a), Some(b)) = (at(a), at(b))
        {
            layout.swappable.push((a, b));
        }
    }
    layout
}

impl fmt::Display for Layout {
    /// `iPhone 17 Pro (iPhone18,1): 0 Bottom 1, 1 Back, 2 Front, 3 Bottom 2`,
    /// with how the names were found.
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match (self.product, &self.model) {
            (Some(product), Some(model)) => write!(f, "{product} ({model})")?,
            (None, Some(model)) => write!(f, "{model}")?,
            _ => write!(f, "an unknown model")?,
        }
        let mics: Vec<String> = self.mics.iter().map(|m| format!("{} {}", m.channel, m.name)).collect();
        write!(f, ": {}", mics.join(", "))?;
        match self.source {
            Source::Model if self.checked => write!(f, " (checked)"),
            Source::Model => write!(f, " (from its device tree)"),
            Source::Default => write!(f, " (not in the map: the usual order)"),
            Source::Numbered => write!(f, " (the map's channel count differs: numbered)"),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

    use super::*;

    #[test]
    fn the_map_is_whole() {
        let maps = maps();
        assert!(maps.models.len() >= 30, "every iPhone iOS 27 supports");
        for (model, entry) in
            maps.models.iter().map(|(m, e)| (m.as_str(), &e.channels)).chain([("default", &maps.default)])
        {
            assert!(entry.iter().all(|code| maps.mics.contains_key(code)), "{model}: a code with no meaning");
            assert_eq!(entry.iter().collect::<BTreeSet<_>>().len(), entry.len(), "{model}: a code twice");
        }
        let ids: BTreeSet<_> = maps.mics.values().map(|m| &m.id).collect();
        assert_eq!(ids.len(), maps.mics.len(), "ids are unique");
    }

    #[test]
    fn the_iphone_17_pro_is_bottom_back_front_bottom() {
        let layout = layout(Some("iPhone18,1"), 4);
        assert_eq!(layout.source, Source::Model);
        assert!(layout.checked);
        assert_eq!(layout.product, Some("iPhone 17 Pro"));
        let ids: Vec<_> = layout.mics.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["bottom-1", "back", "front", "bottom-2"]);
        assert_eq!(layout.swappable, [(0, 3)], "its two bottom microphones may trade places");
        assert_eq!(layout.to_string(), "iPhone 17 Pro (iPhone18,1): 0 Bottom 1, 1 Back, 2 Front, 3 Bottom 2 (checked)");
    }

    #[test]
    fn an_unlisted_model_gets_the_usual_order() {
        let layout = layout(Some("iPhone99,1"), 4);
        assert_eq!(layout.source, Source::Default);
        assert_eq!(layout.mics[1].name, "Back");
        assert_eq!(super::layout(None, 4).source, Source::Default);
    }

    #[test]
    fn another_channel_count_is_numbered() {
        let layout = layout(Some("iPhone18,1"), 2);
        assert_eq!(layout.source, Source::Numbered);
        let ids: Vec<_> = layout.mics.iter().map(|m| m.id.as_str()).collect();
        assert_eq!(ids, ["mic-1", "mic-2"]);
        assert!(layout.swappable.is_empty());
    }
}
