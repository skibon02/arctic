//! # Connection
//!
//! Owns a connected [`Peripheral`] and a single background task that reads the
//! peripheral's notification stream and routes each notification to the
//! interested consumer.
//!
//! btleplug exposes exactly one notification stream per peripheral. Any code
//! that calls `notifications()` and consumes from it competes with every other
//! consumer for the same packets. To avoid that, all notifications are read in
//! one place and dispatched by characteristic UUID:
//!
//! * PMD control point responses are forwarded to a channel the control point
//!   command sender awaits.
//! * PMD data frames are forwarded to the active streaming channel.
//! * PS-FTP frames are forwarded to a channel the PFTP client awaits.
//!
//! The dispatcher also watches the adapter's event stream for
//! `DeviceDisconnected`, which ends all in-flight operations with
//! [`Error::Disconnected`].

use crate::{Error, PolarResult};

use btleplug::api::{Central, CentralEvent, Peripheral as _, ValueNotification};
use btleplug::platform::{Adapter, Peripheral, PeripheralId};
use futures::stream::StreamExt;
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use tokio::sync::{broadcast, mpsc};
use uuid::Uuid;

/// Capacity of the per-consumer notification channels.
const CHANNEL_CAPACITY: usize = 64;

/// A handle to a connected device with a running notification dispatcher.
///
/// Cloning a `Connection` yields another handle to the same connection and
/// dispatcher; the dispatcher stops when the last handle is dropped.
#[derive(Clone)]
pub(crate) struct Connection {
    inner: Arc<ConnectionInner>,
}

struct ConnectionInner {
    device: Peripheral,
    /// Send side of the notification router, keyed by characteristic UUID.
    routes: Mutex<HashMap<Uuid, mpsc::Sender<Vec<u8>>>>,
    /// Broadcast channel signalled when the device disconnects.
    disconnected: broadcast::Sender<()>,
    /// The dispatcher and disconnect-watch tasks, aborted on drop.
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

impl Connection {
    /// Connects to `device`, discovers services, and starts the dispatcher.
    pub(crate) async fn connect(central: &Adapter, device: Peripheral) -> PolarResult<Connection> {
        device.connect().await.map_err(Error::BleError)?;
        device.discover_services().await.map_err(Error::BleError)?;

        let (disconnected, _) = broadcast::channel(1);

        let inner = Arc::new(ConnectionInner {
            device,
            routes: Mutex::new(HashMap::new()),
            disconnected: disconnected.clone(),
            tasks: Mutex::new(Vec::new()),
        });

        let notifications = inner
            .device
            .notifications()
            .await
            .map_err(Error::BleError)?;
        let dispatcher = tokio::spawn(dispatch_notifications(notifications, inner.clone()));

        let disconnect_watch = tokio::spawn(watch_disconnect(
            central.clone(),
            inner.device.id(),
            disconnected,
        ));

        inner
            .tasks
            .lock()
            .unwrap()
            .extend([dispatcher, disconnect_watch]);

        Ok(Connection { inner })
    }

    /// The underlying peripheral.
    pub(crate) fn device(&self) -> &Peripheral {
        &self.inner.device
    }

    /// Returns whether the device is currently connected.
    pub(crate) async fn is_connected(&self) -> bool {
        self.inner.device.is_connected().await.unwrap_or(false)
    }

    /// Subscribes to a characteristic and returns a receiver for its
    /// notifications.
    ///
    /// Only one consumer may be registered per characteristic at a time.
    pub(crate) async fn subscribe(&self, uuid: Uuid) -> PolarResult<mpsc::Receiver<Vec<u8>>> {
        // Only enable notifications on the first subscriber. Re-subscribing to
        // an already-notifying characteristic is unnecessary and can disturb
        // the link on some platforms.
        let already_subscribed = self.inner.routes.lock().unwrap().contains_key(&uuid);
        if !already_subscribed {
            let characteristic = crate::find_characteristic(&self.inner.device, uuid).await?;
            self.inner
                .device
                .subscribe(&characteristic)
                .await
                .map_err(Error::BleError)?;
        }

        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        self.inner.routes.lock().unwrap().insert(uuid, tx);
        log::debug!("arctic: subscribe {uuid}");
        Ok(rx)
    }

    /// Removes the consumer registered for a characteristic and unsubscribes.
    pub(crate) async fn unsubscribe(&self, uuid: Uuid) -> PolarResult<()> {
        self.inner.routes.lock().unwrap().remove(&uuid);
        if let Some(characteristic) = self
            .inner
            .device
            .characteristics()
            .iter()
            .find(|c| c.uuid == uuid)
            .cloned()
        {
            self.inner
                .device
                .unsubscribe(&characteristic)
                .await
                .map_err(Error::BleError)?;
        }
        Ok(())
    }

    /// Subscribes to device disconnection.
    pub(crate) fn disconnected(&self) -> broadcast::Receiver<()> {
        self.inner.disconnected.subscribe()
    }

    /// Disconnects from the device and stops the background tasks.
    pub(crate) async fn disconnect(&self) {
        for task in self.inner.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
        let _ = self.inner.device.disconnect().await;
    }
}

impl Drop for ConnectionInner {
    fn drop(&mut self) {
        for task in self.tasks.lock().unwrap().drain(..) {
            task.abort();
        }
    }
}

/// Reads notifications and forwards each to the consumer registered for its
/// characteristic UUID. Ends when the notification stream ends.
async fn dispatch_notifications(
    mut notifications: std::pin::Pin<Box<dyn futures::Stream<Item = ValueNotification> + Send>>,
    inner: Arc<ConnectionInner>,
) {
    while let Some(notification) = notifications.next().await {
        let sender = inner.routes.lock().unwrap().get(&notification.uuid).cloned();
        match sender {
            // A closed receiver means the consumer has gone away; drop the
            // notification rather than tearing down the dispatcher.
            Some(sender) => {
                if sender.send(notification.value).await.is_err() {
                    log::trace!("arctic: dispatch {} had no receiver", notification.uuid);
                }
            }
            None => log::trace!("arctic: dispatch {} had no route", notification.uuid),
        }
    }

    // The notification stream ended, which indicates the device is gone.
    log::debug!("arctic: notification dispatcher ended; signalling disconnect");
    let _ = inner.disconnected.send(());
}

/// Watches the adapter event stream for this device's disconnection.
async fn watch_disconnect(central: Adapter, id: PeripheralId, disconnected: broadcast::Sender<()>) {
    let events = match central.events().await {
        Ok(events) => events,
        Err(_) => return,
    };
    let mut events = events;

    while let Some(event) = events.next().await {
        if let CentralEvent::DeviceDisconnected(disconnected_id) = event {
            if disconnected_id == id {
                log::debug!("arctic: adapter reported device disconnected");
                let _ = disconnected.send(());
                return;
            }
        }
    }
    log::debug!("arctic: adapter event stream ended");
}
