//! Understands what people have at hand for a server: `user@host:port`, or a whole `ssh` command.
use crate::{ForwardKind, ForwardSpec};
use std::net::IpAddr;

const LOOPBACK: &str = "127.0.0.1";
const ANY_ADDRESS: &str = "0.0.0.0";
const BAD_PORT: &str = "Port must be a number between 1 and 65535";
/// The options of `ssh` that take a value, as in its `getopt` string.
const WITH_VALUE: &str = "BbcDEeFIiJLlmOoPpQRSWw";

/// What the text said about a host. Each field is `None` where it left the choice open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SshCommand {
    pub user: Option<String>,
    /// Empty when the text names no server yet.
    pub host: String,
    pub port: Option<u16>,
    pub identity: Option<String>,
    pub jump: Option<String>,
    pub proxy_command: Option<String>,
    pub forward_agent: bool,
    pub keepalive: Option<u32>,
    pub alive_count: Option<u32>,
    pub forwards: Vec<ForwardSpec>,
    /// What it said that has no place in a host, as written: a remote command, other options.
    pub ignored: Vec<String>,
}

impl SshCommand {
    /// Parses as much as is there, so that it can follow someone typing. Fails only on a port that
    /// cannot be one.
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut words = split_words(text).into_iter().peekable();
        words.next_if(|word| is_ssh(word));
        let mut command = Self::default();
        let mut explicit = Explicit::default();
        let mut destination = None;
        while let Some(word) = words.next() {
            if word == "--" {
                destination = destination.or_else(|| words.next());
                break;
            }
            if let Some(cluster) = word.strip_prefix('-').filter(|rest| !rest.is_empty()) {
                for (index, letter) in cluster.char_indices() {
                    if WITH_VALUE.contains(letter) {
                        let attached = &cluster[index + letter.len_utf8()..];
                        let value = if attached.is_empty() {
                            words.next().unwrap_or_default()
                        } else {
                            attached.to_string()
                        };
                        command.option(letter, value, &mut explicit)?;
                        break;
                    }
                    match letter {
                        'A' => command.forward_agent = true,
                        'a' => command.forward_agent = false,
                        other => command.ignored.push(format!("-{other}")),
                    }
                }
                continue;
            }
            if destination.is_some() {
                // The second word that is no option starts the command to run on the server.
                let rest: Vec<String> = std::iter::once(word).chain(words.by_ref()).collect();
                command.ignored.push(rest.join(" "));
                break;
            }
            destination = Some(word);
        }
        if let Some(destination) = destination {
            let target = parse_target(&destination)?;
            command.host = target.host;
            command.user = explicit.user.or(target.user);
            command.port = explicit.port.or(target.port);
        } else {
            command.user = explicit.user;
            command.port = explicit.port;
        }
        Ok(command)
    }

    /// The server alone, as the address field shows it.
    pub fn address(&self) -> String {
        address_text(self.user.as_deref(), &self.host, self.port)
    }

    fn option(
        &mut self,
        letter: char,
        value: String,
        explicit: &mut Explicit,
    ) -> Result<(), String> {
        match letter {
            'p' => explicit.port = Some(parse_port(&value)?),
            'l' => explicit.user = Some(value),
            'i' => self.identity = Some(value),
            'J' => self.jump = Some(value),
            'L' | 'R' | 'D' => {
                let kind = match letter {
                    'L' => ForwardKind::Local,
                    'R' => ForwardKind::Remote,
                    _ => ForwardKind::Dynamic,
                };
                self.add_forward(kind, &value, letter);
            }
            'o' => self.config_option(&value, explicit)?,
            other => self.ignored.push(format!("-{other} {value}")),
        }
        Ok(())
    }

    /// `-o Name=value` or `-o "Name value"`.
    fn config_option(&mut self, text: &str, explicit: &mut Explicit) -> Result<(), String> {
        let (name, value) = text
            .split_once(['=', ' '])
            .map_or((text, ""), |(name, value)| (name, value.trim()));
        match name.to_ascii_lowercase().as_str() {
            "port" => explicit.port = Some(parse_port(value)?),
            "user" => explicit.user = Some(value.to_string()),
            "identityfile" => self.identity = Some(value.to_string()),
            "proxyjump" => self.jump = Some(value.to_string()),
            "proxycommand" => self.proxy_command = Some(value.to_string()),
            "forwardagent" => self.forward_agent = value.eq_ignore_ascii_case("yes"),
            "serveraliveinterval" if value.parse::<u32>().is_ok() => {
                self.keepalive = value.parse().ok();
            }
            "serveralivecountmax" if value.parse::<u32>().is_ok() => {
                self.alive_count = value.parse().ok();
            }
            "localforward" => self.add_forward(ForwardKind::Local, &spaces_to_colon(value), 'L'),
            "remoteforward" => self.add_forward(ForwardKind::Remote, &spaces_to_colon(value), 'R'),
            "dynamicforward" => self.add_forward(ForwardKind::Dynamic, value, 'D'),
            _ => self.ignored.push(format!("-o {text}")),
        }
        Ok(())
    }

    fn add_forward(&mut self, kind: ForwardKind, value: &str, letter: char) {
        match parse_forward(kind, value) {
            Some(spec) => self.forwards.push(spec),
            None => self.ignored.push(format!("-{letter} {value}")),
        }
    }
}

/// Whether the text says more than a server's address, or says it as a command.
pub fn is_ssh_command(text: &str) -> bool {
    let text = text.trim();
    text.starts_with('-') || text.contains(char::is_whitespace)
}

/// `user@host:port`, leaving out what goes without saying.
pub fn address_text(user: Option<&str>, host: &str, port: Option<u16>) -> String {
    let user = user
        .filter(|user| !user.is_empty())
        .map_or_else(String::new, |user| format!("{user}@"));
    match port.filter(|port| *port != 22) {
        Some(port) if host.contains(':') => format!("{user}[{host}]:{port}"),
        Some(port) => format!("{user}{host}:{port}"),
        None => format!("{user}{host}"),
    }
}

/// The name a server goes by unless told otherwise: `web-01` for `web-01.example.com`.
pub fn suggested_host_name(host: &str) -> String {
    if host.parse::<IpAddr>().is_ok() {
        return host.to_string();
    }
    host.split('.')
        .next()
        .filter(|label| !label.is_empty())
        .unwrap_or(host)
        .to_string()
}

/// What beats the address: `ssh -p 22 -l me you@host:2222` logs in as `me` on port 22.
#[derive(Default)]
struct Explicit {
    user: Option<String>,
    port: Option<u16>,
}

fn is_ssh(word: &str) -> bool {
    let name = word.rsplit(['/', '\\']).next().unwrap_or(word);
    name.eq_ignore_ascii_case("ssh") || name.eq_ignore_ascii_case("ssh.exe")
}

/// Splits on blanks, keeping what is in quotes together. A backslash stays what it is: on Windows
/// it is mostly part of a path.
fn split_words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word: Option<String> = None;
    let mut quote = None;
    for ch in text.chars() {
        match quote {
            Some(open) if ch == open => quote = None,
            Some(_) => word.get_or_insert_default().push(ch),
            None if ch == '"' || ch == '\'' => {
                quote = Some(ch);
                word.get_or_insert_default();
            }
            None if ch.is_whitespace() => words.extend(word.take()),
            None => word.get_or_insert_default().push(ch),
        }
    }
    words.extend(word);
    words
}

fn parse_port(text: &str) -> Result<u16, String> {
    match text.parse() {
        Ok(port) if port > 0 => Ok(port),
        _ => Err(BAD_PORT.to_string()),
    }
}

struct Target {
    user: Option<String>,
    host: String,
    port: Option<u16>,
}

/// `[ssh://][user@]host[:port]`, where an IPv6 address needs brackets only to have a port after it.
fn parse_target(text: &str) -> Result<Target, String> {
    let text = text.strip_prefix("ssh://").unwrap_or(text);
    let text = text.strip_suffix('/').unwrap_or(text);
    let (user, place) = match text.rsplit_once('@') {
        Some((user, place)) => (Some(user.to_string()).filter(|u| !u.is_empty()), place),
        None => (None, text),
    };
    let (host, port) = if let Some(rest) = place.strip_prefix('[') {
        match rest.split_once(']') {
            Some((host, after)) => (host, after.strip_prefix(':')),
            // Still being typed.
            None => (rest, None),
        }
    } else {
        match place.split_once(':') {
            Some((host, port)) if !port.contains(':') => (host, Some(port)),
            // More than one colon: an IPv6 address.
            Some(_) => (place, None),
            None => (place, None),
        }
    };
    // Nothing after the colon yet is someone still typing the port.
    let port = port
        .filter(|port| !port.is_empty())
        .map(parse_port)
        .transpose()?;
    Ok(Target {
        user,
        host: host.to_string(),
        port,
    })
}

fn spaces_to_colon(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(":")
}

/// Splits on colons, except those inside the brackets of an IPv6 address.
fn split_colons(text: &str) -> Vec<&str> {
    let mut parts = Vec::new();
    let (mut start, mut depth) = (0, 0);
    for (index, ch) in text.char_indices() {
        match ch {
            '[' => depth += 1,
            ']' => depth -= 1,
            ':' if depth == 0 => {
                parts.push(&text[start..index]);
                start = index + 1;
            }
            _ => {}
        }
    }
    parts.push(&text[start..]);
    parts
}

fn unbracket(text: &str) -> &str {
    text.trim_matches(['[', ']'])
}

/// An empty address or `*` is every interface, as for `ssh`.
fn bind_address(text: &str) -> String {
    match unbracket(text) {
        "" | "*" => ANY_ADDRESS.to_string(),
        address => address.to_string(),
    }
}

/// `[bind:]port:host:hostport`, or `[bind:]port` for a SOCKS proxy. `None` for what cannot be held,
/// as a Unix socket.
fn parse_forward(kind: ForwardKind, text: &str) -> Option<ForwardSpec> {
    let parts = split_colons(text);
    if kind == ForwardKind::Dynamic {
        let (bind, port) = match parts.as_slice() {
            [port] => (LOOPBACK.to_string(), *port),
            [bind, port] => (bind_address(bind), *port),
            _ => return None,
        };
        return Some(ForwardSpec::socks(bind, parse_port(port).ok()?));
    }
    let (bind, port, host, host_port) = match parts.as_slice() {
        [port, host, host_port] => (LOOPBACK.to_string(), *port, *host, *host_port),
        [bind, port, host, host_port] => (bind_address(bind), *port, *host, *host_port),
        _ => return None,
    };
    Some(ForwardSpec {
        bind_addr: bind,
        bind_port: parse_port(port).ok()?,
        remote_host: unbracket(host).to_string(),
        remote_port: parse_port(host_port).ok()?,
        kind,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(text: &str) -> SshCommand {
        SshCommand::parse(text).unwrap()
    }

    #[test]
    fn a_bare_name_is_a_server() {
        let command = parse("web-01");
        assert_eq!(command.host, "web-01");
        assert_eq!(command.user, None);
        assert_eq!(command.port, None);
    }

    #[test]
    fn an_address_gives_user_host_and_port() {
        let command = parse("root@10.0.0.4:2222");
        assert_eq!(command.user.as_deref(), Some("root"));
        assert_eq!(command.host, "10.0.0.4");
        assert_eq!(command.port, Some(2222));
    }

    #[test]
    fn a_url_is_an_address() {
        let command = parse("ssh://deploy@example.com:2200/");
        assert_eq!(command.address(), "deploy@example.com:2200");
    }

    #[test]
    fn nothing_typed_yet_is_no_error() {
        for text in ["", "   ", "ssh", "ssh ", "root@", "host:", "[::1"] {
            assert!(SshCommand::parse(text).is_ok(), "{text}");
        }
        assert_eq!(parse("root@").host, "");
        assert_eq!(parse("host:").port, None);
    }

    #[test]
    fn a_port_that_cannot_be_one_says_so() {
        for text in [
            "host:0",
            "host:70000",
            "host:ssh",
            "-p abc host",
            "-p 0 host",
        ] {
            assert_eq!(SshCommand::parse(text).unwrap_err(), BAD_PORT, "{text}");
        }
    }

    #[test]
    fn ipv6_addresses_keep_their_colons() {
        assert_eq!(parse("::1").host, "::1");
        assert_eq!(parse("me@fe80::1").host, "fe80::1");
        let command = parse("me@[fe80::1]:2222");
        assert_eq!(command.host, "fe80::1");
        assert_eq!(command.port, Some(2222));
        assert_eq!(command.address(), "me@[fe80::1]:2222");
        assert_eq!(address_text(None, "::1", None), "::1");
    }

    #[test]
    fn a_whole_command_fills_everything_it_can() {
        let command = parse(
            "ssh -p 2222 -i ~/.ssh/work_ed25519 -J bastion -A root@prod-db.example.com \
             -L 5432:db.internal:5432 -R 9000:localhost:9000 -D 1080",
        );
        assert_eq!(command.address(), "root@prod-db.example.com:2222");
        assert_eq!(command.identity.as_deref(), Some("~/.ssh/work_ed25519"));
        assert_eq!(command.jump.as_deref(), Some("bastion"));
        assert!(command.forward_agent);
        assert_eq!(command.ignored, Vec::<String>::new());
        assert_eq!(
            command.forwards,
            [
                ForwardSpec {
                    bind_addr: LOOPBACK.into(),
                    bind_port: 5432,
                    remote_host: "db.internal".into(),
                    remote_port: 5432,
                    kind: ForwardKind::Local,
                },
                ForwardSpec {
                    bind_addr: LOOPBACK.into(),
                    bind_port: 9000,
                    remote_host: "localhost".into(),
                    remote_port: 9000,
                    kind: ForwardKind::Remote,
                },
                ForwardSpec::socks(LOOPBACK.into(), 1080),
            ]
        );
    }

    #[test]
    fn options_can_be_glued_and_clustered() {
        let command = parse("ssh -p2222 -AiC:\\keys\\lab.pem host");
        assert_eq!(command.port, Some(2222));
        assert!(command.forward_agent);
        assert_eq!(command.identity.as_deref(), Some("C:\\keys\\lab.pem"));
        assert_eq!(command.host, "host");
    }

    #[test]
    fn the_flags_win_over_the_address_as_in_ssh() {
        let command = parse("ssh -p 22 -l me you@host:2222");
        assert_eq!(command.user.as_deref(), Some("me"));
        assert_eq!(command.port, Some(22));
    }

    #[test]
    fn config_options_are_understood_either_way() {
        let command = parse(
            "ssh -o ServerAliveInterval=30 -o \"ServerAliveCountMax 5\" -o ForwardAgent=yes \
             -o Port=2200 -o \"ProxyCommand=aws ssm start-session --target %h\" \
             -o \"LocalForward 8080 localhost:80\" host",
        );
        assert_eq!(command.keepalive, Some(30));
        assert_eq!(command.alive_count, Some(5));
        assert!(command.forward_agent);
        assert_eq!(command.port, Some(2200));
        assert_eq!(
            command.proxy_command.as_deref(),
            Some("aws ssm start-session --target %h")
        );
        assert_eq!(command.forwards[0].bind_port, 8080);
        assert_eq!(command.forwards[0].remote_port, 80);
    }

    #[test]
    fn what_has_no_place_in_a_host_is_reported() {
        let command = parse("ssh -v -F other.conf -o Compression=yes host uptime -a");
        assert_eq!(command.host, "host");
        assert_eq!(
            command.ignored,
            ["-v", "-F other.conf", "-o Compression=yes", "uptime -a"]
        );
    }

    #[test]
    fn options_may_follow_the_server() {
        let command = parse("ssh host -p 2222");
        assert_eq!(command.host, "host");
        assert_eq!(command.port, Some(2222));
    }

    #[test]
    fn forwards_can_name_where_they_listen() {
        let any = parse("-L *:8080:db:5432 h").forwards.remove(0);
        assert_eq!(any.bind_addr, ANY_ADDRESS);
        let one = parse("-L 10.0.0.1:8080:[fe80::1]:5432 h")
            .forwards
            .remove(0);
        assert_eq!(one.bind_addr, "10.0.0.1");
        assert_eq!(one.remote_host, "fe80::1");
        let socks = parse("-D 127.0.0.2:1080 h").forwards.remove(0);
        assert_eq!(socks.bind_addr, "127.0.0.2");
        assert_eq!(socks.kind, ForwardKind::Dynamic);
    }

    #[test]
    fn a_forward_that_cannot_be_held_is_reported() {
        let command = parse("-L /tmp/socket:db:5432 -L nonsense h");
        assert!(command.forwards.is_empty());
        assert_eq!(command.ignored, ["-L /tmp/socket:db:5432", "-L nonsense"]);
    }

    #[test]
    fn the_program_may_be_named_with_its_path() {
        assert_eq!(
            parse("C:\\Windows\\System32\\OpenSSH\\ssh.exe host").host,
            "host"
        );
    }

    #[test]
    fn a_command_is_told_from_an_address() {
        assert!(!is_ssh_command("root@host:22"));
        assert!(!is_ssh_command("  host  "));
        assert!(is_ssh_command("ssh host"));
        assert!(is_ssh_command("host -p 22"));
        assert!(is_ssh_command("-p 22 host"));
    }

    #[test]
    fn port_22_goes_without_saying() {
        assert_eq!(address_text(Some("me"), "host", Some(22)), "me@host");
        assert_eq!(address_text(Some(""), "host", Some(2200)), "host:2200");
    }

    #[test]
    fn a_name_comes_from_the_first_label_of_a_domain() {
        assert_eq!(suggested_host_name("web-01.example.com"), "web-01");
        assert_eq!(suggested_host_name("web-01"), "web-01");
        assert_eq!(suggested_host_name("10.0.0.4"), "10.0.0.4");
        assert_eq!(suggested_host_name("fe80::1"), "fe80::1");
    }
}
