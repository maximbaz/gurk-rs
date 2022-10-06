use std::borrow::Cow;
use std::fs::File;
use std::io::{BufReader, BufWriter};
use std::path::{Path, PathBuf};

use anyhow::Context;
use tracing::{error, info};

use crate::data::{AppData, Channel, ChannelId, Message};

use super::{MessageId, Storage};

// TODO: In memory only, does not save!
pub struct JsonStorage {
    data_path: PathBuf,
    data: AppData,
}

impl JsonStorage {
    pub fn new(data_path: &Path, fallback_data_path: Option<&Path>) -> anyhow::Result<Self> {
        let mut data_path = data_path;
        if !data_path.exists() {
            // try also to load from a fallback (legacy) data path
            if let Some(fallback_data_path) = fallback_data_path.as_ref() {
                data_path = fallback_data_path;
            }
        }

        // if data file exists, be conservative and fail rather than overriding and losing the messages
        let mut data = if data_path.exists() {
            Self::load_data_from(&data_path).with_context(|| {
                format!(
                    "failed to load stored data from '{}':\n\
            This might happen due to incompatible data model when Gurk is upgraded.\n\
            Please consider to backup your messages and then remove the store.",
                    data_path.display()
                )
            })?
        } else {
            Self::load_data_from(data_path).unwrap_or_default()
        };

        // invariant: messages are sorted by arrived_at
        for channel in &mut data.channels.items {
            channel
                .messages
                .items
                .sort_unstable_by_key(|msg| msg.arrived_at);
        }

        Ok(Self {
            data_path: data_path.into(),
            data,
        })
    }

    fn load_data_from(data_path: &Path) -> anyhow::Result<AppData> {
        info!("loading app data from: {}", data_path.display());
        let f = BufReader::new(File::open(data_path)?);
        Ok(serde_json::from_reader(f)?)
    }

    fn save(&self) -> anyhow::Result<()> {
        let f = BufWriter::new(File::create(&self.data_path)?);
        Ok(serde_json::to_writer(f, &self.data)?)
    }
}

impl Storage for JsonStorage {
    fn channels<'s>(&'s self) -> Box<dyn Iterator<Item = Cow<Channel>> + 's> {
        Box::new(self.data.channels.items.iter().map(Cow::Borrowed))
    }

    fn channel(&self, channel_id: ChannelId) -> Option<Cow<Channel>> {
        self.data
            .channels
            .items
            .iter()
            .find(|channel| channel.id == channel_id)
            .map(Cow::Borrowed)
    }

    fn store_channel(&mut self, channel: Channel) -> Cow<Channel> {
        let channel_idx = if let Some(idx) = self
            .data
            .channels
            .items
            .iter_mut()
            .position(|ch| ch.id == channel.id)
        {
            let stored_channel = &mut self.data.channels.items[idx];
            *stored_channel = channel;
            idx
        } else {
            self.data.channels.items.push(channel);
            self.data.channels.items.len() - 1
        };
        if let Err(e) = self.save() {
            error!(error =% e, "failed to save storage");
        }
        Cow::Borrowed(&self.data.channels.items[channel_idx])
    }

    fn messages<'s>(
        &'s self,
        channel_id: ChannelId,
    ) -> Box<dyn Iterator<Item = Cow<Message>> + 's> {
        if let Some(channel) = self
            .data
            .channels
            .items
            .iter()
            .find(|ch| ch.id == channel_id)
        {
            Box::new(channel.messages.items.iter().map(Cow::Borrowed))
        } else {
            Box::new(std::iter::empty())
        }
    }

    fn message(&self, message_id: MessageId) -> Option<Cow<Message>> {
        let channel = self
            .data
            .channels
            .items
            .iter()
            .find(|ch| ch.id == message_id.channel_id)?;
        let message = channel
            .messages
            .items
            .iter()
            .find(|message| message.arrived_at == message_id.arrived_at)?;
        Some(Cow::Borrowed(message))
    }

    fn store_message(&mut self, channel_id: ChannelId, message: Message) -> Cow<Message> {
        let channel_idx = self
            .data
            .channels
            .items
            .iter()
            .position(|ch| ch.id == channel_id)
            .expect("no such channel");
        let idx = {
            let channel = &mut self.data.channels.items[channel_idx];
            match channel
                .messages
                .items
                .binary_search_by_key(&message.arrived_at, |msg| msg.arrived_at)
            {
                Ok(idx) => {
                    let stored_message = &mut channel.messages.items[idx];
                    *stored_message = message;
                    idx
                }
                Err(idx) => {
                    channel.messages.items.insert(idx, message);
                    idx
                }
            }
        };
        if let Err(e) = self.save() {
            error!(error = %e, "failed to save storage");
        }
        Cow::Borrowed(&self.data.channels.items[channel_idx].messages.items[idx])
    }
}

#[cfg(test)]
mod tests {
    use tempfile::NamedTempFile;
    use uuid::Uuid;

    use crate::data::TypingSet;
    use crate::util::{FilteredStatefulList, StatefulList};

    use super::*;

    #[test]
    fn test_json_storage_data_model() {
        let user_id1: Uuid = "966960e0-a8cd-43f1-ac7a-2c986dd470cd".parse().unwrap();
        let user_id2: Uuid = "a955d20f-6b83-4e69-846e-a99b1779ff7a".parse().unwrap();
        let user_id3: Uuid = "ac9b8aa1-691a-47e1-a566-d3e942945d07".parse().unwrap();
        let channel1 = Channel {
            id: user_id1.into(),
            name: "direct-channel".to_string(),
            group_data: None,
            messages: StatefulList::with_items(vec![Message {
                from_id: user_id2,
                message: Some("hello".into()),
                arrived_at: 1664832050000,
                quote: Default::default(),
                attachments: Default::default(),
                reactions: Default::default(),
                receipt: Default::default(),
            }]),
            unread_messages: 1,
            typing: TypingSet::SingleTyping(false),
        };
        let channel2 = Channel {
            id: ChannelId::Group(*b"4149b9686807fdb4a8c95d9b5413bbcd"),
            name: "group-channel".to_string(),
            group_data: None,
            messages: StatefulList::with_items(vec![Message {
                from_id: user_id3,
                message: Some("world".into()),
                arrived_at: 1664832050001,
                quote: Default::default(),
                attachments: Default::default(),
                reactions: Default::default(),
                receipt: Default::default(),
            }]),
            unread_messages: 2,
            typing: TypingSet::GroupTyping(Default::default()),
        };
        let data = AppData {
            channels: FilteredStatefulList::_with_items(vec![channel1, channel2]),
            names: Default::default(),
            contacts_sync_request_at: Default::default(),
        };
        insta::assert_json_snapshot!(data);
    }

    fn json_storage_from_snapshot() -> impl Storage {
        let json =
            include_str!("snapshots/gurk__storage2__json__tests__json_storage_data_model.snap")
                .rsplit("---")
                .next()
                .unwrap();
        let f = NamedTempFile::new().unwrap();
        std::fs::write(&f, json.as_bytes()).unwrap();

        JsonStorage::new(f.path(), None).unwrap()
    }

    #[test]
    fn test_json_storage_channels() {
        let storage = json_storage_from_snapshot();
        let channels: Vec<_> = storage.channels().collect();
        assert_eq!(channels.len(), 2);
        assert_eq!(storage.channel(channels[0].id).unwrap().id, channels[0].id);
        assert_eq!(storage.channel(channels[1].id).unwrap().id, channels[1].id);
    }

    #[test]
    fn test_json_storage_store_existing_channel() {
        let mut storage = json_storage_from_snapshot();
        let id: Uuid = "966960e0-a8cd-43f1-ac7a-2c986dd470cd".parse().unwrap();
        let mut channel = storage.channel(id.into()).unwrap().into_owned();
        channel.name = "new name".to_string();
        channel.unread_messages = 23;

        let stored_channel = storage.store_channel(channel);
        assert_eq!(stored_channel.id, id.into());
        assert_eq!(stored_channel.name, "new name");
        assert_eq!(stored_channel.unread_messages, 23);

        let channels: Vec<_> = storage.channels().collect();
        assert_eq!(channels.len(), 2);
        assert_eq!(storage.channel(channels[0].id).unwrap().id, channels[0].id);
        assert_eq!(storage.channel(channels[1].id).unwrap().id, channels[1].id);

        let channel = storage.channel(channels[0].id).unwrap();
        assert_eq!(channel.id, id.into());
        assert_eq!(channel.name, "new name");
        assert_eq!(channel.unread_messages, 23);
    }

    #[test]
    fn test_json_storage_store_new_channel() {
        let mut storage = json_storage_from_snapshot();
        let id: Uuid = "e3690a5f-70a4-4a49-8125-ca689adb2d9e".parse().unwrap();
        storage.store_channel(Channel {
            id: id.into(),
            name: "test".to_string(),
            group_data: None,
            messages: Default::default(),
            unread_messages: 42,
            typing: TypingSet::SingleTyping(false),
        });
        let channels: Vec<_> = storage.channels().collect();
        assert_eq!(channels.len(), 3);
        assert_eq!(storage.channel(channels[0].id).unwrap().id, channels[0].id);
        assert_eq!(storage.channel(channels[1].id).unwrap().id, channels[1].id);
        let channel = storage.channel(channels[2].id).unwrap();
        assert_eq!(channel.id, id.into());
        assert_eq!(channel.name, "test");
        assert_eq!(channel.unread_messages, 42);
    }

    #[test]
    fn test_json_storage_messages() {
        let storage = json_storage_from_snapshot();
        let id: Uuid = "966960e0-a8cd-43f1-ac7a-2c986dd470cd".parse().unwrap();

        let messages: Vec<_> = storage.messages(id.into()).collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].message.as_deref(), Some("hello"));

        let arrived_at = messages[0].arrived_at;
        let message = storage
            .message(MessageId::new(id.into(), arrived_at))
            .unwrap();
        assert_eq!(message.arrived_at, arrived_at);
        assert_eq!(message.message.as_deref(), Some("hello"));
    }

    #[test]
    fn test_json_storage_store_existing_message() {
        let mut storage = json_storage_from_snapshot();
        let id: Uuid = "966960e0-a8cd-43f1-ac7a-2c986dd470cd".parse().unwrap();
        let arrived_at = 1664832050000;
        let mut message = storage
            .message(MessageId::new(id.into(), arrived_at))
            .unwrap()
            .into_owned();
        message.message = Some("changed".to_string());

        let arrived_at = message.arrived_at;
        let stored_message = storage.store_message(id.into(), message);
        assert_eq!(stored_message.arrived_at, arrived_at);
        assert_eq!(stored_message.message.as_deref(), Some("changed"));

        let messages: Vec<_> = storage.messages(id.into()).collect();
        assert_eq!(messages.len(), 1);
        assert_eq!(messages[0].arrived_at, arrived_at);
        assert_eq!(messages[0].message.as_deref(), Some("changed"));
    }

    #[test]
    fn test_json_storage_store_new_message() {
        let mut storage = json_storage_from_snapshot();
        let id: Uuid = "966960e0-a8cd-43f1-ac7a-2c986dd470cd".parse().unwrap();
        let arrived_at = 1664832050001;
        assert_eq!(storage.message(MessageId::new(id.into(), arrived_at)), None);

        let stored_message = storage.store_message(
            id.into(),
            Message {
                from_id: id,
                message: Some("new msg".to_string()),
                arrived_at,
                quote: Default::default(),
                attachments: Default::default(),
                reactions: Default::default(),
                receipt: Default::default(),
            },
        );

        assert_eq!(stored_message.arrived_at, arrived_at);
        assert_eq!(stored_message.message.as_deref(), Some("new msg"));

        let messages: Vec<_> = storage.messages(id.into()).collect();
        assert_eq!(messages.len(), 2);
        assert_eq!(messages[1].arrived_at, arrived_at);
        assert_eq!(messages[1].message.as_deref(), Some("new msg"));
    }
}