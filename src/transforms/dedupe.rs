use super::Transform;
use crate::{
    event::{Event, Value},
    runtime::TaskExecutor,
    topology::config::{DataType, TransformConfig, TransformDescription},
};
use bytes::Bytes;
use chrono::{DateTime, Utc};
use lru::LruCache;
use serde::{Deserialize, Serialize};
use std::any::TypeId;
use string_cache::DefaultAtom as Atom;

#[derive(Deserialize, Serialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct FieldMatchConfig {
    #[serde(rename = "match")]
    pub match_fields: Vec<Atom>,
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct CacheConfig {
    pub num_entries: usize,
}

#[derive(Deserialize, Serialize, Debug)]
#[serde(deny_unknown_fields)]
pub struct DedupeConfig {
    #[serde(default = "default_field_match_config")]
    pub fields: FieldMatchConfig,
    #[serde(default = "default_cache_config")]
    pub cache: CacheConfig,
}

fn default_cache_config() -> CacheConfig {
    CacheConfig { num_entries: 5000 }
}

fn default_field_match_config() -> FieldMatchConfig {
    FieldMatchConfig {
        match_fields: vec!["timestamp".into()],
    }
}

pub struct Dedupe {
    config: DedupeConfig,
    cache: LruCache<CacheEntry, bool>,
}

inventory::submit! {
    TransformDescription::new_without_default::<DedupeConfig>("dedupe")
}

#[typetag::serde(name = "dedupe")]
impl TransformConfig for DedupeConfig {
    fn build(&self, _exec: TaskExecutor) -> crate::Result<Box<dyn Transform>> {
        Ok(Box::new(Dedupe::new(
            self.cache.num_entries,
            self.fields.match_fields.clone(),
        )))
    }

    fn input_type(&self) -> DataType {
        DataType::Log
    }

    fn output_type(&self) -> DataType {
        DataType::Log
    }

    fn transform_type(&self) -> &'static str {
        "dedupe"
    }
}

type CacheEntry = Vec<(TypeId, Bytes)>;

fn type_id_for_value(val: &Value) -> TypeId {
    match val {
        Value::Bytes(_) => TypeId::of::<Bytes>(),
        Value::Timestamp(_) => TypeId::of::<DateTime<Utc>>(),
        Value::Integer(_) => TypeId::of::<i64>(),
        Value::Float(_) => TypeId::of::<f64>(),
        Value::Boolean(_) => TypeId::of::<bool>(),
    }
}

impl Dedupe {
    pub fn new(num_entries: usize, match_fields: Vec<Atom>) -> Self {
        Self {
            config: DedupeConfig {
                fields: FieldMatchConfig { match_fields },
                cache: CacheConfig { num_entries },
            },
            cache: LruCache::new(num_entries),
        }
    }

    /**
     * Takes in an Event and returns a new Event with only the fields that are being matched against.
     **/
    pub fn extract_matched_fields(&self, event: &Event) -> CacheEntry {
        let mut entry = CacheEntry::new();

        for field_name in self.config.fields.match_fields.iter() {
            if let Some(value) = event.as_log().get(field_name) {
                entry.push((type_id_for_value(&value), value.as_bytes()));
            }
        }

        entry
    }
}

impl Transform for Dedupe {
    fn transform(&mut self, event: Event) -> Option<Event> {
        let cache_entry = self.extract_matched_fields(&event);
        if self.cache.put(cache_entry, true).is_some() {
            None
        } else {
            Some(event)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Dedupe;
    use crate::{event::Event, transforms::Transform};

    #[test]
    fn dedupe_test_basic_matching() {
        let mut event1 = Event::from("message");
        event1.as_mut_log().insert("matched", "some value");
        event1.as_mut_log().insert("unmatched", "another value");

        // Test that unmatched field isn't considered
        let mut event2 = Event::from("message");
        event2.as_mut_log().insert("matched", "some value2");
        event2.as_mut_log().insert("unmatched", "another value");

        // Test that matched field is considered
        let mut event3 = Event::from("message");
        event3.as_mut_log().insert("matched", "some value");
        event3.as_mut_log().insert("unmatched", "another value2");

        let mut transform = Dedupe::new(5, vec!["matched".into()]);

        // First event should always be passed through as-is.
        let new_event = transform.transform(event1).unwrap();
        assert_eq!(new_event.as_log()[&"matched".into()], "some value".into());

        // Second event differs in matched field so should be outputted even though it
        // has the same value for unmatched field.
        let new_event = transform.transform(event2).unwrap();
        assert_eq!(new_event.as_log()[&"matched".into()], "some value2".into());

        // Third event has the same value for "matched" as first event, so it should be dropped.
        assert_eq!(None, transform.transform(event3));
    }

    /**
     * Test that two Events that are considered duplicates get handled that way, even
     * if the order of the matched fields is different between the two.
     * Note that this test is a bit non-deterministic in its ability to test the underlying
     * issue (meaning it's possible for this test to pass even if the bug of the transform
     * caring about field order is present).  This is difficult to avoid because Event is
     * backed by a HashMap and there's no way to deterministically control what order the
     * objects will be stored in table underlying the HashMap.
     **/
    #[test]
    fn dedupe_test_field_order_irrelevant() {
        let mut event1 = Event::from("message");
        event1.as_mut_log().insert("matched1", "value1");
        event1.as_mut_log().insert("randomData", "ignored"); // Helps force field order to be different more often
        event1.as_mut_log().insert("matched2", "value2");

        // Add fields in opposite order
        let mut event2 = Event::from("message");
        event2.as_mut_log().insert("matched2", "value2");
        event2.as_mut_log().insert("matched1", "value1");

        let mut transform = Dedupe::new(5, vec!["matched1".into(), "matched2".into()]);

        // First event should always be passed through as-is.
        let new_event = transform.transform(event1).unwrap();
        assert_eq!(new_event.as_log()[&"matched1".into()], "value1".into());
        assert_eq!(new_event.as_log()[&"matched2".into()], "value2".into());

        // Second event is the same just with different field order, so it shouldn't be outputted.
        assert_eq!(None, transform.transform(event2));
    }

    #[test]
    fn dedupe_test_age_out() {
        let mut event1 = Event::from("message");
        event1.as_mut_log().insert("matched", "some value");

        let mut event2 = Event::from("message");
        event2.as_mut_log().insert("matched", "some value2");

        // This event is a duplicate of event1, but won't be treated as such.
        let event3 = event1.clone();

        // Construct transform with a cache size of only 1 entry.
        let mut transform = Dedupe::new(1, vec!["matched".into()]);

        // First event should always be passed through as-is.
        let new_event = transform.transform(event1).unwrap();
        assert_eq!(new_event.as_log()[&"matched".into()], "some value".into());

        // Second event gets outputted because it's not a dupe.  This causes the first
        // Event to be evicted from the cache.
        let new_event = transform.transform(event2).unwrap();
        assert_eq!(new_event.as_log()[&"matched".into()], "some value2".into());

        // Third event is a dupe but gets outputted anyway because the first event has aged
        // out of the cache.
        let new_event = transform.transform(event3).unwrap();
        assert_eq!(new_event.as_log()[&"matched".into()], "some value".into());
    }

    /**
     * Test that two events with values for the matched fields that have different
     * types but the same string representation aren't considered duplicates.
     */
    #[test]
    fn dedupe_test_type_matching() {
        let mut event1 = Event::from("message");
        event1.as_mut_log().insert("matched", "123");

        let mut event2 = Event::from("message");
        event2.as_mut_log().insert("matched", 123);

        let mut transform = Dedupe::new(5, vec!["matched".into()]);

        // First event should always be passed through as-is.
        let new_event = transform.transform(event1).unwrap();
        assert_eq!(new_event.as_log()[&"matched".into()], "123".into());

        // Second event should also get passed through even though the string representations of
        // "matched" are the same.
        let new_event = transform.transform(event2).unwrap();
        assert_eq!(new_event.as_log()[&"matched".into()], 123.into());
    }
}
