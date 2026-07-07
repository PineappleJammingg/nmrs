# Real-Time Monitoring

nmrs uses D-Bus signals to provide real-time notifications when network state changes. This is more efficient than polling — your callback fires only when something actually changes.

Both `monitor_network_changes()` and `monitor_device_changes()` return a
[`MonitorHandle`](../api/types.md) you can use to shut the monitor down cleanly.

## Network Change Monitoring

Subscribe to network changes (access points appearing or disappearing, or signal
strength changing):

```rust
use nmrs::NetworkManager;

#[tokio::main]
async fn main() -> nmrs::Result<()> {
    let nm = NetworkManager::new().await?;

    let handle = nm.monitor_network_changes(|| {
        println!("Network list changed!");
    }).await?;

    // ... application work ...

    handle.stop().await?;
    Ok(())
}
```

`monitor_network_changes()` subscribes to D-Bus signals for access point additions, removals, and signal strength updates on all Wi-Fi devices. The callback fires whenever the visible network list or signal data changes.

## Device State Monitoring

Subscribe to device state changes (connected, disconnected, cable plugged in, etc.):

```rust
use nmrs::NetworkManager;

#[tokio::main]
async fn main() -> nmrs::Result<()> {
    let nm = NetworkManager::new().await?;

    let handle = nm.monitor_device_changes(|| {
        println!("Device state changed!");
    }).await?;

    // ... application work ...

    handle.stop().await?;
    Ok(())
}
```

`monitor_device_changes()` subscribes to state change signals on all network devices — both wired and wireless.

## Running Monitors as Background Tasks

In a real application, spawn monitors as background tasks and keep the returned
`MonitorHandle` so you can stop them on shutdown:

### With Tokio

```rust
use nmrs::NetworkManager;

#[tokio::main]
async fn main() -> nmrs::Result<()> {
    let nm = NetworkManager::new().await?;

    let net_handle = {
        let nm = nm.clone();
        tokio::spawn(async move {
            nm.monitor_network_changes(|| {
                println!("Networks changed");
            }).await
        })
    };

    let dev_handle = {
        let nm = nm.clone();
        tokio::spawn(async move {
            nm.monitor_device_changes(|| {
                println!("Device state changed");
            }).await
        })
    };

    // Your main application logic here
    loop {
        tokio::time::sleep(std::time::Duration::from_secs(60)).await;
    }

    // On shutdown:
    // net_handle.await??.stop().await?;
    // dev_handle.await??.stop().await?;
}
```

### With GTK/GLib (for GUI applications)

```rust
use nmrs::NetworkManager;

// Inside a GTK application
let nm = NetworkManager::new().await?;

let net_handle = glib::MainContext::default().spawn_local({
    let nm = nm.clone();
    async move { nm.monitor_network_changes(|| {
        println!("Networks changed — refresh the UI!");
    }).await }
});

let dev_handle = glib::MainContext::default().spawn_local({
    let nm = nm.clone();
    async move { nm.monitor_device_changes(|| {
        println!("Device changed — update status!");
    }).await }
});

// Keep `net_handle` / `dev_handle` alive, then call `MonitorHandle::stop()`
// when tearing down the UI.
```

## Thread Safety

`NetworkManager` is `Clone` and can be safely shared across async tasks. Each clone shares the same underlying D-Bus connection, making it lightweight to pass into multiple monitoring tasks.

## Practical Pattern: Refresh on Change

A common pattern is to refresh your application state whenever a change is detected:

```rust
use nmrs::NetworkManager;
use std::sync::Arc;
use tokio::sync::Notify;

#[tokio::main]
async fn main() -> nmrs::Result<()> {
    let nm = NetworkManager::new().await?;
    let notify = Arc::new(Notify::new());

    let handle = {
        let notify = notify.clone();
        let nm = nm.clone();
        tokio::spawn(async move {
            nm.monitor_network_changes(move || {
                notify.notify_one();
            }).await
        })
    };

    loop {
        notify.notified().await;

        let networks = nm.list_networks(None).await?;
        println!("Updated: {} networks visible", networks.len());
    }

    // handle.await??.stop().await?;
}
```

## What Triggers Each Monitor

| Monitor | Triggers |
|---------|----------|
| `monitor_network_changes` | Access point added, access point removed, signal strength change |
| `monitor_device_changes` | Device state change (connected, disconnected, etc.), cable plug/unplug |

## Next Steps

- [Device Management](./devices.md) – understand device states
- [WiFi Management](./wifi.md) – scan and connect to networks
- [Error Handling](./error-handling.md) – handle monitoring errors
