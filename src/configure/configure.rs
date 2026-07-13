use std::collections::HashSet;
use std::io;
use std::sync::atomic::Ordering;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::time::Duration;

use crate::broadcast::{self, DiscoveredHost};
use crate::config::{Config, InputDevice, PeerConfig};
use crate::constants::LOGO;
use crate::crypto::validate_public_key;
use crate::errors::error;
use crate::errors::{Error, ErrorKind, Result};
use crate::scroll;
use super::constants::HOST_POLL_INTERVAL;
use super::prompt::Prompt;
use super::utils::{
    INTERRUPTED,
    handle_interrupt,
    interrupted,
    is_root,
    ensure_port,
    print_host_prompt,
};

pub async fn configure(mut config: Config, scroll: bool) -> Result<()> {
    println!("{}", LOGO);

    let prompt = Prompt::start();
    let scroll_allowed = scroll || is_root();

    let interfaces = broadcast::list_interfaces()?;
    if interfaces.is_empty() {
        return Err(Error::new(ErrorKind::Network)
            .with_msg("configure: No network interfaces found"));
    }

    println!("Network interfaces:");
    for interface in &interfaces {
        println!("  {}", interface);
    }

    let default_interface = broadcast::default_interface(&interfaces);
    let interface = loop {
        let name = match &default_interface {
            Some(default) => prompt.line_default(
                "Enter interface name for broadcast", default)?,
            None => prompt.line("Enter interface name for broadcast: ")?,
        };

        match interfaces.iter().find(|i| i.name == name) {
            Some(interface) => break interface.clone(),
            None => println!("Unknown interface {}", name),
        }
    };

    let default_address = if config.server.address.is_empty() {
        interface.address.to_string()
    } else {
        config.server.address.clone()
    };

    let mut address = prompt.line_default(
        "Hostname for this system", &default_address)?;
    while address.is_empty() {
        address = prompt.line_default(
            "Hostname for this system", &default_address)?;
    }
    config.server.address = address;

    config.server.clipboard_source =
        prompt.yes_no_default("Enable clipboard source", true)?;
    config.server.clipboard_destination =
        prompt.yes_no_default("Enable clipboard destination", true)?;
    if scroll_allowed {
        config.server.scroll_destination =
            prompt.yes_no_default("Enable scroll destination", false)?;
        config.server.scroll_source = if config.server.scroll_destination {
            println!("Skipping scroll source, scroll destination is enabled");
            false
        } else {
            prompt.yes_no_default("Enable scroll source", false)?
        };
    } else {
        println!("Skipping scroll configuration, not running as root, \
            use --scroll to override");
    }

    if scroll_allowed &&
        (config.server.scroll_source || config.server.scroll_destination)
    {
        println!();
        println!("Scroll on each device to detect it, press Enter when done");
        let detected = scroll::detect_devices_interactive(&prompt.lines)?;

        let mut devices = Vec::new();
        for name in &detected {
            let keep = if config.server.scroll_source {
                prompt.yes_no(&format!("Send scroll events from {}", name))?
            } else {
                prompt.yes_no(&format!("Block scroll events from {}", name))?
            };

            if keep {
                devices.push(InputDevice {
                    name: Some(name.clone()),
                    ..Default::default()
                });
            }
        }

        let existing = std::mem::take(&mut config.server.scroll_input_devices);
        for device in existing {
            let prompted = device.name.as_ref()
                .is_some_and(|name| detected.contains(name));
            if prompted {
                continue;
            }

            let name = device.name.as_deref()
                .or(device.path.as_deref())
                .unwrap_or("unknown");
            if prompt.yes_no(&format!("Keep scroll device {}", name))? {
                devices.push(device);
            }
        }

        config.server.scroll_input_devices = devices;
    }

    println!();
    let bind_port = config.server.bind_port()?;
    let announce = ensure_port(&config.server.address, bind_port);

    if let Err(e) = broadcast::start_key_listener(
        &config.server.bind,
        announce.clone(),
        config.server.public_key.clone(),
    ) {
        error(&e);
        println!(
            "Unable to listen on {}, other hosts cannot fetch this host's public key",
            config.server.bind,
        );
    }

    let discovered = broadcast::start_discovery(
        interface.broadcast,
        bind_port,
        announce,
        config.server.public_key.clone(),
    )?;

    println!("Broadcasting, listening for hosts...");
    let new_peers = add_hosts(
        &prompt,
        &discovered,
        &config.server.public_key,
        bind_port,
        scroll_allowed,
    )?;

    let existing = std::mem::take(&mut config.peers);
    let mut peers = Vec::new();
    for peer in existing {
        let added = new_peers.iter().any(|p| {
            p.address == peer.address || p.public_key == peer.public_key
        });
        if added {
            continue;
        }

        if !prompt.yes_no(&format!("Keep host {}", peer.address))? {
            continue;
        }

        peers.push(prompt_peer_settings(&prompt, peer, scroll_allowed)?);
    }
    peers.extend(new_peers);
    config.peers = peers;

    config.save().await?;
    println!("Configuration saved");

    Ok(())
}

fn add_hosts(
    prompt: &Prompt,
    discovered: &Receiver<DiscoveredHost>,
    own_public_key: &str,
    bind_port: u16,
    scroll_allowed: bool,
) -> Result<Vec<PeerConfig>> {
    let mut peers: Vec<PeerConfig> = Vec::new();

    INTERRUPTED.store(false, Ordering::SeqCst);
    let old_handler = unsafe {
        libc::signal(
            libc::SIGINT,
            handle_interrupt as *const () as libc::sighandler_t,
        )
    };
    if old_handler == libc::SIG_ERR {
        return Err(Error::wrap(io::Error::last_os_error(), ErrorKind::Exec)
            .with_msg("configure: Failed to set interrupt handler"));
    }

    let result = add_hosts_loop(
        prompt, discovered, own_public_key, bind_port, scroll_allowed,
        &mut peers);

    unsafe { libc::signal(libc::SIGINT, old_handler) };
    INTERRUPTED.store(false, Ordering::SeqCst);

    if let Err(e) = result {
        if !e.is_kind(ErrorKind::Cancelled) {
            return Err(e);
        }
        println!();
    }

    Ok(peers)
}

fn add_hosts_loop(
    prompt: &Prompt,
    discovered: &Receiver<DiscoveredHost>,
    own_public_key: &str,
    bind_port: u16,
    scroll_allowed: bool,
    peers: &mut Vec<PeerConfig>,
) -> Result<()> {
    let mut seen: HashSet<String> = HashSet::new();

    print_host_prompt()?;

    loop {
        if interrupted() {
            return Err(Error::new(ErrorKind::Cancelled)
                .with_msg("configure: Host adding interrupted"));
        }

        let mut host_handled = false;
        while let Ok(host) = discovered.try_recv() {
            if host.public_key == own_public_key
                || seen.contains(&host.address)
                || seen.contains(&host.public_key)
            {
                continue;
            }
            seen.insert(host.address.clone());
            seen.insert(host.public_key.clone());
            host_handled = true;

            println!();
            if let Err(e) = validate_public_key(&host.public_key) {
                let e = e.with_ctx("address", host.address.clone());
                error(&e);
                println!(
                    "Ignoring detected host {}, invalid public key",
                    host.address,
                );
                continue;
            }

            println!("Detected host {}", host.address);
            if prompt.yes_no_default(&format!("Add host {}", host.address), true)? {
                let peer = PeerConfig {
                    address: host.address,
                    public_key: host.public_key,
                    ..Default::default()
                };
                peers.push(prompt_peer_settings(prompt, peer, scroll_allowed)?);
            }
        }
        if host_handled {
            print_host_prompt()?;
        }

        let address = match prompt.lines
            .recv_timeout(Duration::from_millis(HOST_POLL_INTERVAL))
        {
            Ok(line) => line,
            Err(RecvTimeoutError::Timeout) => continue,
            Err(RecvTimeoutError::Disconnected) => return Ok(()),
        };

        if address.is_empty() {
            print_host_prompt()?;
            continue;
        }

        let address = ensure_port(&address, bind_port);
        if seen.contains(&address) {
            println!("Host already added");
            print_host_prompt()?;
            continue;
        }

        println!("Fetching public key from {}...", address);
        let fetched = match broadcast::fetch_host_info(&address) {
            Ok(info) => match validate_public_key(&info.public_key) {
                Ok(()) => {
                    println!("Fetched public key {}", info.public_key);
                    Some(info.public_key)
                }
                Err(e) => {
                    let e = e.with_ctx("address", address.clone());
                    error(&e);
                    None
                }
            },
            Err(e) => {
                error(&e);
                None
            }
        };

        let public_key = match fetched {
            Some(key) => key,
            None => {
                let key = loop {
                    let key = prompt.line(&format!(
                        "Unable to fetch public key, enter public key for {}: ",
                        address,
                    ))?;
                    if key.is_empty() {
                        break key;
                    }

                    match validate_public_key(&key) {
                        Ok(()) => break key,
                        Err(e) => {
                            error(&e);
                            println!("Invalid public key");
                        }
                    }
                };
                if key.is_empty() {
                    println!("Skipping host, no public key entered");
                    print_host_prompt()?;
                    continue;
                }
                key
            }
        };

        if public_key == own_public_key {
            println!("Skipping host, public key matches this system");
            print_host_prompt()?;
            continue;
        }

        seen.insert(address.clone());
        seen.insert(public_key.clone());
        let peer = PeerConfig {
            address,
            public_key,
            ..Default::default()
        };
        peers.push(prompt_peer_settings(prompt, peer, scroll_allowed)?);
        print_host_prompt()?;
    }
}

fn prompt_peer_settings(
    prompt: &Prompt,
    mut peer: PeerConfig,
    scroll_allowed: bool,
) -> Result<PeerConfig> {
    peer.clipboard_source = prompt.yes_no_default(
        &format!("{}: Enable clipboard source", peer.address), true)?;
    peer.clipboard_destination = prompt.yes_no_default(
        &format!("{}: Enable clipboard destination", peer.address), true)?;

    if scroll_allowed {
        peer.scroll_source = prompt.yes_no_default(
            &format!("{}: Enable scroll source", peer.address), false)?;
        peer.scroll_destination = if peer.scroll_source {
            println!("Skipping scroll destination, scroll source is enabled");
            false
        } else {
            prompt.yes_no_default(
                &format!("{}: Enable scroll destination", peer.address), false)?
        };
    }

    Ok(peer)
}
