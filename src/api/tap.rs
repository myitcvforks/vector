use super::{ControlMessage, ControlSender};
use crate::{
    event::{Event, LogEvent},
    topology::fanout::RouterSink,
};
use futures::{channel::mpsc, SinkExt, StreamExt};
use parking_lot::RwLock;
use std::{
    collections::HashMap,
    hash::{Hash, Hasher},
    sync::Arc,
};
use uuid::Uuid;

type TapSender = mpsc::UnboundedSender<TapResult>;

pub enum TapError {
    ComponentInvalid,
    ComponentGoneAway,
}

pub enum TapResult {
    LogEvent(String, LogEvent),
    Error(String, TapError),
    Stop,
}

pub enum TapControl {
    Start(Arc<TapSink>),
    Stop(Arc<TapSink>),
}

pub struct TapSink {
    id: Uuid,
    inputs: RwLock<HashMap<String, Uuid>>,
    tap_tx: TapSender,
}

impl TapSink {
    /// Creates a new tap sink, and spawn a listener per sink
    pub fn new(input_names: &[String], tap_tx: TapSender) -> Self {
        // Map each input name to a UUID
        let inputs = input_names
            .iter()
            .map(|name| (name.to_string(), Uuid::new_v4()))
            .collect();

        Self {
            id: Uuid::new_v4(),
            inputs: RwLock::new(inputs),
            tap_tx,
        }
    }

    pub fn input_names(&self) -> Vec<String> {
        self.inputs.read().keys().cloned().collect()
    }

    pub fn inputs(&self) -> HashMap<String, Uuid> {
        self.inputs
            .read()
            .iter()
            .map(|(name, uuid)| (name.to_string(), *uuid))
            .collect()
    }

    /// Internal function to build a `RouterSink` from an input name. This will spawn an async
    /// task to forward on `LogEvent`s to the tap channel.
    fn make_router(&self, input_name: &str) -> RouterSink {
        let (event_tx, mut event_rx) = mpsc::unbounded();
        let mut tap_tx = self.tap_tx.clone();
        let input_name = input_name.to_string();

        tokio::spawn(async move {
            while let Some(ev) = event_rx.next().await {
                if let Event::Log(ev) = ev {
                    let _ = tap_tx.start_send(TapResult::LogEvent(input_name.clone(), ev));
                }
            }
        });

        Box::new(event_tx.sink_map_err(|_| ()))
    }

    pub fn make_output(&self, input_name: &str) -> Result<(String, RouterSink), ()> {
        let lock = self.inputs.read();
        let id = lock.get(input_name).ok_or(())?;

        Ok((id.to_string(), self.make_router(input_name)))
    }

    fn remove_input_result(&self, input_name: &str, result: TapResult) -> Option<Uuid> {
        let mut lock = self.inputs.write();
        let id = lock.remove(input_name)?;
        let _ = self.tap_tx.clone().start_send(result);

        if lock.is_empty() {
            let _ = self.tap_tx.clone().start_send(TapResult::Stop);
        }

        Some(id)
    }

    pub fn component_invalid(&self, input_name: &str) -> Option<Uuid> {
        self.remove_input_result(
            input_name,
            TapResult::Error(input_name.to_string(), TapError::ComponentInvalid),
        )
    }

    pub fn component_gone_away(&self, input_name: &str) -> Option<Uuid> {
        self.remove_input_result(
            input_name,
            TapResult::Error(input_name.to_string(), TapError::ComponentGoneAway),
        )
    }
}

impl Hash for TapSink {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.id.hash(state)
    }
}

impl PartialEq for TapSink {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}

impl Eq for TapSink {}

pub struct TapController {
    control_tx: ControlSender,
    sink: Arc<TapSink>,
}

impl TapController {
    pub fn new(control_tx: ControlSender, sink: TapSink) -> Self {
        let sink = Arc::new(sink);

        let _ = control_tx.send(ControlMessage::Tap(TapControl::Start(Arc::clone(&sink))));
        Self { control_tx, sink }
    }
}

impl Drop for TapController {
    fn drop(&mut self) {
        let _ = self
            .control_tx
            .send(ControlMessage::Tap(TapControl::Stop(Arc::clone(
                &self.sink,
            ))));
    }
}
