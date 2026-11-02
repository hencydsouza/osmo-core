use std::net::Ipv4Addr;

/// The camera is only reachable over its own WiFi. `local` is the address the OS would use to reach it
/// (from a connected UDP socket); if that is not on the camera's /24, traffic would go elsewhere.
pub fn route_warning(camera_ip: &str, local: Option<Ipv4Addr>) -> Option<String> {
    let cam: Ipv4Addr = camera_ip.parse().ok()?;
    match local {
        Some(l) if l.octets()[..3] == cam.octets()[..3] => None,
        Some(l) => Some(format!(
            "traffic to {camera_ip} would leave via {l}, which is not on the camera's network: join the camera's WiFi (see `osmo wifi`)"
        )),
        None => Some(format!("no route to {camera_ip}: join the camera's WiFi (see `osmo wifi`)")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::Ipv4Addr;

    #[test]
    fn no_warning_when_local_address_is_on_the_camera_subnet() {
        assert_eq!(
            route_warning("192.168.2.1", Some(Ipv4Addr::new(192, 168, 2, 5))),
            None
        );
    }

    #[test]
    fn warns_when_traffic_would_leave_via_another_network() {
        let w = route_warning("192.168.2.1", Some(Ipv4Addr::new(192, 168, 1, 20))).unwrap();
        assert!(w.contains("192.168.2.1") && w.contains("WiFi"), "{w}");
    }

    #[test]
    fn warns_when_there_is_no_route_at_all() {
        assert!(route_warning("192.168.2.1", None)
            .unwrap()
            .contains("192.168.2.1"));
    }

    #[test]
    fn non_ipv4_camera_address_gives_no_warning() {
        assert_eq!(route_warning("camera.local", None), None);
    }
}
