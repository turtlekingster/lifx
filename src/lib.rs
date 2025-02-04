pub mod bulb_manager {
    
    use get_if_addrs::{get_if_addrs, IfAddr, Ifv4Addr};
    use lifx_core::{
        get_product_info, BuildOptions, Message, PowerLevel, RawMessage, Service, HSBK,
    };
    use std::collections::HashMap;
    use std::ffi::CString;
    use std::net::{IpAddr, SocketAddr, UdpSocket};
    
    use std::sync::{Arc, Mutex};
    use std::thread::spawn;
    use std::time::{Duration, Instant};

    const HOUR: Duration = Duration::from_secs(60 * 60);

    #[derive(Debug)]
    pub struct RefreshableData<T> {
        pub data: Option<T>,
        pub max_age: Duration,
        pub last_updated: Instant,
        pub refresh_msg: Message,
    }

    impl<T> RefreshableData<T> {
        fn empty(max_age: Duration, refresh_msg: Message) -> RefreshableData<T> {
            RefreshableData {
                data: None,
                max_age,
                last_updated: Instant::now(),
                refresh_msg,
            }
        }
        fn update(&mut self, data: T) {
            self.data = Some(data);
            self.last_updated = Instant::now()
        }
        fn needs_refresh(&self) -> bool {
            self.data.is_none() || self.last_updated.elapsed() > self.max_age
        }
        fn as_ref(&self) -> Option<&T> {
            self.data.as_ref()
        }
    }
    pub struct Zones {
        pub zones_count: u16,
        zone_index: u16,
        colors_count: u8,
        colors: Box<[HSBK; 82]>,
    }
    pub struct BulbInfo {
        pub last_seen: Instant,
        pub options: BuildOptions,
        pub addr: SocketAddr,
        pub name: RefreshableData<CString>,
        pub model: RefreshableData<(u32, u32)>,
        pub location: RefreshableData<CString>,
        pub host_firmware: RefreshableData<(u16, u16)>,
        pub wifi_firmware: RefreshableData<(u16, u16)>,
        pub power_level: RefreshableData<u16>,
        pub zones: RefreshableData<Zones>,
        pub color: Color,
    }

    #[derive(Debug)]
    pub enum Color {
        Unknown,
        Single(RefreshableData<HSBK>),
        Multi(RefreshableData<Vec<Option<HSBK>>>),
    }

    impl BulbInfo {
        fn new(source: u32, target: u64, addr: SocketAddr) -> BulbInfo {
            println!("New bulb at: {:?}", addr);
            BulbInfo {
                last_seen: Instant::now(),
                addr,
                options: BuildOptions {
                    target: Some(target),
                    ack_required: true,
                    res_required: true,
                    source: source,
                    sequence: 0,
                },
                name: RefreshableData::empty(HOUR, Message::GetLabel),
                model: RefreshableData::empty(HOUR, Message::GetVersion),
                location: RefreshableData::empty(HOUR, Message::GetLocation),
                host_firmware: RefreshableData::empty(HOUR, Message::GetHostFirmware),
                wifi_firmware: RefreshableData::empty(HOUR, Message::GetWifiFirmware),
                power_level: RefreshableData::empty(Duration::from_secs(15), Message::GetPower),
                zones: RefreshableData::empty(
                    Duration::from_secs(15),
                    Message::GetExtendedColorZones,
                ),
                color: Color::Unknown,
            }
        }
        pub fn get_colors(&self) -> Result<Box<[HSBK; 82]>, failure::Error>{
            Ok(self.zones.as_ref().unwrap().colors.clone())
        }
        pub fn get_length(&self) -> Result<u32, failure::Error>{
            Ok(self.zones.as_ref().unwrap().zones_count.clone().into())
        }

        fn update(&mut self, addr: SocketAddr) {
            self.last_seen = Instant::now();
            self.addr = addr;
        }

        fn refresh_if_needed<T>(
            &self,
            sock: &UdpSocket,
            data: &RefreshableData<T>,
        ) -> Result<(), failure::Error> {
            if data.needs_refresh() {
                let message: RawMessage =
                    RawMessage::build(&self.options, data.refresh_msg.clone())?;
                sock.send_to(&message.pack()?, self.addr)?;
            }
            Ok(())
        }

        pub fn toggle_bulb(&self, sock: &UdpSocket) -> Result<(), failure::Error> {
            let payload: Message;
            if let Some(level) = self.power_level.as_ref() {
                if *level > 0 {
                    payload = Message::SetPower {
                        level: lifx_core::PowerLevel::Standby,
                    };
                } else {
                    payload = Message::SetPower {
                        level: lifx_core::PowerLevel::Enabled,
                    };
                }
            } else {
                payload = Message::SetPower {
                    level: lifx_core::PowerLevel::Enabled,
                };
            }
            let message: RawMessage = RawMessage::build(&self.options, payload)?;
            sock.send_to(&message.pack()?, self.addr)?;
            Ok(())
        }

        pub fn set_power_duration(
            &self,
            sock: &UdpSocket,
            level: u16,
            duration: u32,
        ) -> Result<(), failure::Error> {
            let payload: Message = Message::LightSetPower {
                level: level,
                duration: duration,
            };
            let message: RawMessage = RawMessage::build(&self.options, payload)?;
            sock.send_to(&message.pack()?, self.addr)?;
            Ok(())
        }

        pub fn set_power(&self, sock: &UdpSocket, level: PowerLevel) -> Result<(), failure::Error> {
            let payload: Message = Message::SetPower { level: level };
            let message: RawMessage = RawMessage::build(&self.options, payload)?;
            sock.send_to(&message.pack()?, self.addr)?;
            Ok(())
        }

        pub fn set_bulb_color(
            &self,
            sock: &UdpSocket,
            color: HSBK,
            duration: u32,
        ) -> Result<(), failure::Error> {
            let payload: Message = Message::LightSetColor {
                reserved: 0,
                color: color,
                duration: duration,
            };
            let message: RawMessage = RawMessage::build(&self.options, payload)?;
            sock.send_to(&message.pack()?, self.addr)?;
            Ok(())
        }
        pub fn set_strip_array(
            &self,
            sock: &UdpSocket,
            colors: Box<[HSBK; 82]>,
            duration: u32,
        ) -> Result<(), failure::Error> {
            if let Some(zones) = self.zones.as_ref() {
                let payload: Message = Message::SetExtendedColorZones {
                    duration: duration,
                    apply: lifx_core::ApplicationRequest::Apply,
                    zone_index: 0,
                    colors_count: zones.colors_count,
                    colors: colors,
                };
                // println!("{:?}", payload);
                let message: RawMessage = RawMessage::build(&self.options, payload)?;
                sock.send_to(&message.pack()?, self.addr)?;
            }
            Ok(())
        }

        fn query_for_missing_info(&self, sock: &UdpSocket) -> Result<(), failure::Error> {
            self.refresh_if_needed(sock, &self.name)?;
            self.refresh_if_needed(sock, &self.model)?;
            self.refresh_if_needed(sock, &self.location)?;
            self.refresh_if_needed(sock, &self.host_firmware)?;
            self.refresh_if_needed(sock, &self.wifi_firmware)?;
            self.refresh_if_needed(sock, &self.power_level)?;
            match &self.color {
                Color::Unknown => (), // we'll need to wait to get info about this bulb's model, so we'll know if it's multizone or not
                Color::Single(d) => self.refresh_if_needed(sock, d)?,
                Color::Multi(d) => self.refresh_if_needed(sock, d)?,
            }
            if let Some((vendor, product)) = self.model.as_ref() {
                if let Some(info) = get_product_info(*vendor, *product) {
                    if info.extended {
                        self.refresh_if_needed(sock, &self.zones)?;
                    }
                }
            }
            Ok(())
        }
    }

    impl std::fmt::Debug for BulbInfo {
        fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            write!(
                f,
                "BulbInfo({:0>16X} - {}  ",
                self.options.target.unwrap(),
                self.addr
            )?;

            if let Some(name) = self.name.as_ref() {
                write!(f, "{}", name.to_string_lossy())?;
            }
            if let Some(location) = self.location.as_ref() {
                write!(f, "/{}", location.to_string_lossy())?;
            }
            if let Some((vendor, product)) = self.model.as_ref() {
                if let Some(info) = get_product_info(*vendor, *product) {
                    write!(f, " - {} ", info.name)?;
                    write!(f, " - {} ", product)?;
                } else {
                    write!(
                        f,
                        " - Unknown model (vendor={}, product={}) ",
                        vendor, product
                    )?;
                }
            }
            if let Some((major, minor)) = self.host_firmware.as_ref() {
                write!(f, " McuFW:{}.{}", major, minor)?;
            }
            if let Some((major, minor)) = self.wifi_firmware.as_ref() {
                write!(f, " WifiFW:{}.{}", major, minor)?;
            }
            if let Some(level) = self.power_level.as_ref() {
                if *level > 0 {
                    write!(f, "  Powered On(")?;
                    match self.color {
                        Color::Unknown => write!(f, "??")?,
                        Color::Single(ref color) => {
                            f.write_str(
                                &color
                                    .as_ref()
                                    .map(|c| c.describe(false))
                                    .unwrap_or_else(|| "??".to_owned()),
                            )?;
                        }
                        Color::Multi(ref color) => {
                            if let Some(vec) = color.as_ref() {
                                write!(f, "Zones: ")?;
                                for zone in vec {
                                    if let Some(color) = zone {
                                        write!(f, "{} ", color.describe(true))?;
                                    } else {
                                        write!(f, "?? ")?;
                                    }
                                }
                            }
                        }
                    }
                    write!(f, ")")?;
                } else {
                    write!(f, "  Powered Off")?;
                }
            }
            if let Some((vendor, product)) = self.model.as_ref() {
                if let Some(info) = get_product_info(*vendor, *product) {
                    if info.extended {
                        if let Some(zones) = self.zones.as_ref() {
                            write!(
                                f,
                                "(ZC:{}, ZI:{}, ZCC:{})",
                                zones.zones_count, zones.zone_index, zones.colors_count
                            )?;
                        }
                    }
                }
            }
            write!(f, ")")
        }
    }

    pub struct Manager {
        pub bulbs: Arc<Mutex<HashMap<u64, BulbInfo>>>,
        pub last_discovery: Instant,
        pub sock: UdpSocket,
        source: u32,
    }

    impl Manager {
        pub fn new() -> Result<Manager, failure::Error> {
            let sock: UdpSocket = UdpSocket::bind("0.0.0.0:56700")?;
            sock.set_broadcast(true)?;

            // spawn a thread that can send to our socket
            let recv_sock: UdpSocket = sock.try_clone()?;

            let bulbs: Arc<Mutex<HashMap<u64, BulbInfo>>> = Arc::new(Mutex::new(HashMap::new()));
            let receiver_bulbs: Arc<Mutex<HashMap<u64, BulbInfo>>> = bulbs.clone();
            let source: u32 = 0x72757374;

            // spawn a thread that will receive data from our socket and update our internal data structures
            spawn(move || Self::worker(recv_sock, source, receiver_bulbs));

            let mgr: Manager = Manager {
                bulbs,
                last_discovery: Instant::now(),
                sock,
                source,
            };
            Ok(mgr)
        }

        pub fn handle_message(
            raw: RawMessage,
            bulb: &mut BulbInfo,
        ) -> Result<(), lifx_core::Error> {
            match Message::from_raw(&raw)? {
                Message::StateService { port, service } => {
                    if port != bulb.addr.port() as u32 || service != Service::UDP {
                        println!("Unsupported service: {:?}/{}", service, port);
                    }
                }
                Message::StateLabel { label } => bulb.name.update(label.cstr().to_owned()),
                Message::StateLocation { label, .. } => {
                    bulb.location.update(label.cstr().to_owned())
                }
                Message::StateVersion {
                    vendor, product, ..
                } => {
                    bulb.model.update((vendor, product));
                    if let Some(info) = get_product_info(vendor, product) {
                        if info.multizone {
                            bulb.color = Color::Multi(RefreshableData::empty(
                                Duration::from_secs(15),
                                Message::GetColorZones {
                                    start_index: 0,
                                    end_index: 255,
                                },
                            ))
                        } else {
                            bulb.color = Color::Single(RefreshableData::empty(
                                Duration::from_secs(15),
                                Message::LightGet,
                            ))
                        }
                    }
                }
                Message::StatePower { level } => bulb.power_level.update(level),
                Message::StateHostFirmware {
                    version_minor,
                    version_major,
                    ..
                } => bulb.host_firmware.update((version_major, version_minor)),
                Message::StateWifiFirmware {
                    version_minor,
                    version_major,
                    ..
                } => bulb.wifi_firmware.update((version_major, version_minor)),
                Message::LightState {
                    color,
                    power,
                    label,
                    ..
                } => {
                    if let Color::Single(ref mut d) = bulb.color {
                        d.update(color);
                        bulb.power_level.update(power);
                    }
                    bulb.name.update(label.cstr().to_owned());
                }
                Message::StateZone {
                    count,
                    index,
                    color,
                } => {
                    if let Color::Multi(ref mut d) = bulb.color {
                        d.data.get_or_insert_with(|| {
                            let mut v = Vec::with_capacity(count as usize);
                            v.resize(count as usize, None);
                            assert!(index <= count);
                            v
                        })[index as usize] = Some(color);
                    }
                }
                Message::StateMultiZone {
                    count,
                    index,
                    color0,
                    color1,
                    color2,
                    color3,
                    color4,
                    color5,
                    color6,
                    color7,
                } => {
                    if let Color::Multi(ref mut d) = bulb.color {
                        let v = d.data.get_or_insert_with(|| {
                            let mut v = Vec::with_capacity(count as usize);
                            v.resize(count as usize, None);
                            assert!(index + 7 <= count);
                            v
                        });

                        v[index as usize] = Some(color0);
                        v[index as usize + 1] = Some(color1);
                        v[index as usize + 2] = Some(color2);
                        v[index as usize + 3] = Some(color3);
                        v[index as usize + 4] = Some(color4);
                        v[index as usize + 5] = Some(color5);
                        v[index as usize + 6] = Some(color6);
                        v[index as usize + 7] = Some(color7);
                    }
                }
                Message::StateExtendedColorZones {
                    zones_count,
                    zone_index,
                    colors_count,
                    colors,
                } => {
                    bulb.zones.update(Zones {
                        zones_count: zones_count,
                        zone_index: zone_index,
                        colors_count: colors_count,
                        colors: colors,
                    });
                    // if let Some(zones) = bulb.zones.as_ref() {
                    //     println!("state: {:?}", zones.colors);
                    // }
                }
                Message::Acknowledgement { seq } => {
                    bulb.options.sequence = (seq % 255) + 1;
                    //println!("Awk: {} {}", bulb.addr, bulb.options.sequence);
                }
                unknown => {
                    println!("Received, but ignored {:?}", unknown);
                }
            }
            Ok(())
        }

        pub fn worker(
            recv_sock: UdpSocket,
            source: u32,
            receiver_bulbs: Arc<Mutex<HashMap<u64, BulbInfo>>>,
        ) {
            let mut buf = [0; 1024];
            loop {
                match recv_sock.recv_from(&mut buf) {
                    Ok((0, addr)) => println!("Received a zero-byte datagram from {:?}", addr),
                    Ok((nbytes, addr)) => match RawMessage::unpack(&buf[0..nbytes]) {
                        Ok(raw) => {
                            if raw.frame_addr.target == 0 {
                                continue;
                            }
                            if let Ok(mut bulbs) = receiver_bulbs.lock() {
                                let bulb = bulbs
                                    .entry(raw.frame_addr.target)
                                    .and_modify(|bulb| bulb.update(addr))
                                    .or_insert_with(|| {
                                        BulbInfo::new(source, raw.frame_addr.target, addr)
                                    });
                                if let Err(e) = Self::handle_message(raw, bulb) {
                                    println!("Error handling message from {}: {}", addr, e)
                                }
                            }
                        }
                        Err(e) => println!("Error unpacking raw message from {}: {}", addr, e),
                    },
                    Err(e) => panic!("recv_from err {:?}", e),
                }
            }
        }

        pub fn discover(&mut self) -> Result<(), failure::Error> {
            println!("Doing discovery");

            let opts = BuildOptions {
                source: self.source,
                ..Default::default()
            };
            let rawmsg = RawMessage::build(&opts, Message::GetService).unwrap();
            let bytes = rawmsg.pack().unwrap();

            for addr in get_if_addrs().unwrap() {
                if let IfAddr::V4(Ifv4Addr {
                    broadcast: Some(bcast),
                    ..
                }) = addr.addr
                {
                    if addr.ip().is_loopback() {
                        continue;
                    }
                    let addr = SocketAddr::new(IpAddr::V4(bcast), 56700);
                    println!("Discovering bulbs on LAN {:?}", addr);
                    self.sock.send_to(&bytes, &addr)?;
                }
            }

            self.last_discovery = Instant::now();

            Ok(())
        }

        pub fn refresh(&self) {
            if let Ok(bulbs) = self.bulbs.lock() {
                let bulbs = bulbs.values();
                for bulb in bulbs {
                    bulb.query_for_missing_info(&self.sock).unwrap();
                }
            }
        }

        pub fn add_bulb(&mut self, addr: SocketAddr) -> Result<(), failure::Error> {
            let opts = BuildOptions {
                source: self.source,
                ..Default::default()
            };
            let rawmsg = RawMessage::build(&opts, Message::GetService).unwrap();
            let bytes = rawmsg.pack().unwrap();
            println!("Attempting connection to: {:?}", addr);
            self.sock.send_to(&bytes, &addr)?;
            Ok(())
        }

    }
}
