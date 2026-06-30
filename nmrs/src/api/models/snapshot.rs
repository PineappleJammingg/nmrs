//! Point-in-time NetworkManager snapshot model.

use super::{
    AccessPoint, ActiveConnection, AirplaneModeState, ConnectivityReport, Device, RadioState,
    SavedConnection, WifiDevice,
};

/// Point-in-time state needed by GUI network applets.
///
/// Build this with [`NetworkManager::snapshot`](crate::NetworkManager::snapshot)
/// after receiving a [`NetworkEvent`](super::NetworkEvent).
#[non_exhaustive]
#[derive(Debug, Clone)]
pub struct NetworkSnapshot {
    /// Wi-Fi radio state.
    pub wifi: RadioState,
    /// WWAN/mobile broadband radio state.
    pub wwan: RadioState,
    /// Bluetooth radio state.
    pub bluetooth: RadioState,
    /// Aggregated airplane-mode state.
    pub airplane_mode: AirplaneModeState,
    /// Connectivity and captive-portal state.
    pub connectivity: ConnectivityReport,
    /// Active connections classified for applet rendering.
    pub active_connections: Vec<ActiveConnection>,
    /// Visible access points, one entry per BSSID.
    pub access_points: Vec<AccessPoint>,
    /// All saved connection profiles.
    pub saved_connections: Vec<SavedConnection>,
    /// Saved Wi-Fi profiles.
    pub saved_wifi_profiles: Vec<SavedConnection>,
    /// Saved VPN profiles, including kernel WireGuard profiles.
    pub saved_vpn_profiles: Vec<SavedConnection>,
    /// Wi-Fi device summaries.
    pub wifi_devices: Vec<WifiDevice>,
    /// Wired Ethernet devices from the broad device model.
    pub wired_devices: Vec<Device>,
}

pub(crate) fn saved_wifi_profiles(saved: &[SavedConnection]) -> Vec<SavedConnection> {
    saved
        .iter()
        .filter(|profile| profile.connection_type == "802-11-wireless")
        .cloned()
        .collect()
}

pub(crate) fn saved_vpn_profiles(saved: &[SavedConnection]) -> Vec<SavedConnection> {
    saved
        .iter()
        .filter(|profile| matches!(profile.connection_type.as_str(), "vpn" | "wireguard"))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::models::SettingsSummary;
    use zvariant::OwnedObjectPath;

    fn saved(connection_type: &str) -> SavedConnection {
        SavedConnection {
            path: OwnedObjectPath::try_from("/org/freedesktop/NetworkManager/Settings/1")
                .expect("valid object path"),
            uuid: format!("{connection_type}-uuid"),
            id: format!("{connection_type}-id"),
            connection_type: connection_type.to_string(),
            interface_name: None,
            autoconnect: true,
            autoconnect_priority: 0,
            timestamp_unix: 0,
            permissions: Vec::new(),
            unsaved: false,
            filename: None,
            summary: SettingsSummary::Other {
                sections: vec!["connection".into()],
            },
        }
    }

    #[test]
    fn filters_saved_wifi_profiles() {
        let profiles = vec![
            saved("802-11-wireless"),
            saved("vpn"),
            saved("802-3-ethernet"),
        ];

        let wifi = saved_wifi_profiles(&profiles);

        assert_eq!(wifi.len(), 1);
        assert_eq!(wifi[0].connection_type, "802-11-wireless");
    }

    #[test]
    fn filters_saved_vpn_profiles() {
        let profiles = vec![saved("802-11-wireless"), saved("vpn"), saved("wireguard")];

        let vpn = saved_vpn_profiles(&profiles);

        assert_eq!(vpn.len(), 2);
        assert!(vpn.iter().any(|profile| profile.connection_type == "vpn"));
        assert!(
            vpn.iter()
                .any(|profile| profile.connection_type == "wireguard")
        );
    }
}
