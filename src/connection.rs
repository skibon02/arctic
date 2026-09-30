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
//! * PMD data frames are forwarded to every active streaming channel.
//! * PS-FTP frames are forwarded to a channel the PFTP client awaits.
//!
//! A characteristic may have several consumers at once: multiple concurrent
//! streams all subscribe to the PMD data characteristic and each decodes only
//! the frames of its own measurement type. The dispatcher therefore fans each
//! notification out to every consumer registered for its UUID.
//!
//! The dispatcher also watches the adapter's event stream for
//! `DeviceDisconnected`, which ends all in-flight operations with
//! [`Error::Disconnected`].

use crate::{Error, PolarResult};

use btleplug::api::{Central, CentralEvent, Peripheral as _, ValueNotification};
use btleplug::platform::{Adapter, Peripheral, PeripheralId};
use futures::stream::StreamExt;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
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
    /// Send sides of the notification router, keyed by characteristic UUID.
    /// A UUID maps to every consumer subscribed to it, each identified by a
    /// unique token; each notification is fanned out to all of them.
    routes: Mutex<HashMap<Uuid, Vec<Route>>>,
    /// Serializes control point commands. The control point has a single
    /// response channel shared by every consumer, so only one command may be
    /// in flight at a time; otherwise two commands could read each other's
    /// response.
    control_point: tokio::sync::Mutex<()>,
    /// Source of unique subscription tokens.
    next_token: AtomicU64,
    /// Broadcast channel signalled when the device disconnects.
    disconnected: broadcast::Sender<()>,
    /// The dispatcher and disconnect-watch tasks, aborted on drop.
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
}

/// A single consumer registered for a characteristic's notifications.
#[derive(Clone)]
struct Route {
    token: u64,
    tx: mpsc::Sender<Vec<u8>>,
}

/// A live subscription to a characteristic's notifications.
///
/// Dropping the subscription removes the consumer and, if it was the last one,
/// unsubscribes from the characteristic.
pub(crate) struct Subscription {
    conn: Connection,
    uuid: Uuid,
    token: u64,
    rx: mpsc::Receiver<Vec<u8>>,
}

impl Subscription {
    /// Receives the next notification, or `None` once the connection ends.
    pub(crate) async fn recv(&mut self) -> Option<Vec<u8>> {
        self.rx.recv().await
    }
}

impl Drop for Subscription {
    fn drop(&mut self) {
        let conn = self.conn.clone();
        let uuid = self.uuid;
        let token = self.token;
        // The release unsubscribes from the characteristic, which is async, so
        // it cannot run inline in `drop`. Spawn it; the connection outlives the
        // subscription through the cloned handle.
        tokio::spawn(async move {
            conn.release(uuid, token).await;
        });
    }
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
            control_point: tokio::sync::Mutex::new(()),
            next_token: AtomicU64::new(0),
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

    /// Subscribes to a characteristic and returns a subscription whose receiver
    /// yields the characteristic's notifications.
    ///
    /// Several consumers may subscribe to the same characteristic; each
    /// receives every notification for it. This is what allows multiple
    /// measurement streams to share the PMD data characteristic. The
    /// subscription is released when the returned [`Subscription`] is dropped.
    pub(crate) async fn subscribe(&self, uuid: Uuid) -> PolarResult<Subscription> {
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

        let token = self.inner.next_token.fetch_add(1, Ordering::Relaxed);
        let (tx, rx) = mpsc::channel(CHANNEL_CAPACITY);
        self.inner
            .routes
            .lock()
            .unwrap()
            .entry(uuid)
            .or_default()
            .push(Route { token, tx });
        log::debug!("arctic: subscribe {uuid} (token {token})");

        Ok(Subscription {
            conn: self.clone(),
            uuid,
            token,
            rx,
        })
    }

    /// Removes the consumer identified by `token` from `uuid`.
    ///
    /// The characteristic is only unsubscribed once its last consumer is gone.
    async fn release(&self, uuid: Uuid, token: u64) {
        self.remove_routes(uuid, |route| route.token == token).await;
        log::debug!("arctic: unsubscribe {uuid} (token {token})");
    }

    /// Removes every consumer registered for `uuid` and unsubscribes.
    ///
    /// Used to stop a characteristic that is owned as a whole rather than by
    /// individual consumers, such as the standard heart rate measurement.
    pub(crate) async fn release_all(&self, uuid: Uuid) {
        self.remove_routes(uuid, |_| true).await;
        log::debug!("arctic: unsubscribe {uuid} (all)");
    }

    /// Drops the routes matching `predicate` and unsubscribes from the
    /// characteristic once none remain.
    async fn remove_routes<F>(&self, uuid: Uuid, predicate: F)
    where
        F: Fn(&Route) -> bool,
    {
        let last = {
            let mut routes = self.inner.routes.lock().unwrap();
            match routes.get_mut(&uuid) {
                Some(consumers) => {
                    consumers.retain(|route| !predicate(route));
                    if consumers.is_empty() {
                        routes.remove(&uuid);
                        true
                    } else {
                        false
                    }
                }
                None => false,
            }
        };

        if last {
            if let Some(characteristic) = self
                .inner
                .device
                .characteristics()
                .iter()
                .find(|c| c.uuid == uuid)
                .cloned()
            {
                if let Err(err) = self.inner.device.unsubscribe(&characteristic).await {
                    log::debug!("arctic: unsubscribe {uuid} failed: {err:?}");
                }
            }
        }
    }

    /// Subscribes to device disconnection.
    pub(crate) fn disconnected(&self) -> broadcast::Receiver<()> {
        self.inner.disconnected.subscribe()
    }

    /// Acquires the control point lock, serializing control point commands.
    pub(crate) async fn lock_control_point(&self) -> tokio::sync::MutexGuard<'_, ()> {
        self.inner.control_point.lock().await
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

/// Reads notifications and forwards each to every consumer registered for its
/// characteristic UUID. Ends when the notification stream ends.
async fn dispatch_notifications(
    mut notifications: std::pin::Pin<Box<dyn futures::Stream<Item = ValueNotification> + Send>>,
    inner: Arc<ConnectionInner>,
) {
    while let Some(notification) = notifications.next().await {
        let senders = inner
            .routes
            .lock()
            .unwrap()
            .get(&notification.uuid)
            .cloned()
            .unwrap_or_default();

        if senders.is_empty() {
            log::trace!("arctic: dispatch {} had no route", notification.uuid);
            continue;
        }

        for route in senders {
            // A closed receiver means the consumer has gone away; drop the
            // notification for it rather than tearing down the dispatcher.
            if route.tx.send(notification.value.clone()).await.is_err() {
                log::trace!("arctic: dispatch {} had no receiver", notification.uuid);
            }
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
