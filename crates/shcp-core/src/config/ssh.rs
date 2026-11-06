use crate::{ForwardKind, ForwardSpec, HostConfig, HostId, HostSource};
use anyhow::{Context, Result, bail};
use std::{
    collections::{BTreeSet, HashSet},
    path::{Path, PathBuf},
};

/// Quotes and comments are recognized without treating Windows backslashes as escapes.
fn words(line: &str) -> Result<Vec<String>> {
    let mut result = Vec::new();
    let mut word = String::new();
    let mut quote = None;
    for ch in line.chars() {
        match ch {
            '#' if quote.is_none() => break,
            '\'' | '"' if quote == Some(ch) => quote = None,
            '\'' | '"' if quote.is_none() => quote = Some(ch),
            c if c.is_whitespace() && quote.is_none() => {
                if !word.is_empty() {
                    result.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if quote.is_some() {
        bail!("Unclosed quote");
    }
    if !word.is_empty() {
        result.push(word);
    }
    Ok(result)
}

pub(crate) fn wildcard(pattern: &str, value: &str) -> bool {
    let p = pattern.to_ascii_lowercase().into_bytes();
    let v = value.to_ascii_lowercase().into_bytes();
    let (mut i, mut j, mut star, mut retry) = (0, 0, None, 0);
    while j < v.len() {
        if i < p.len() && (p[i] == b'?' || p[i] == v[j]) {
            i += 1;
            j += 1;
        } else if i < p.len() && p[i] == b'*' {
            star = Some(i);
            i += 1;
            retry = j;
        } else if let Some(s) = star {
            retry += 1;
            j = retry;
            i = s + 1;
        } else {
            return false;
        }
    }
    while i < p.len() && p[i] == b'*' {
        i += 1;
    }
    i == p.len()
}
fn matches(patterns: &[String], alias: &str) -> bool {
    let mut positive = false;
    for pattern in patterns {
        if let Some(p) = pattern.strip_prefix('!') {
            if wildcard(p, alias) {
                return false;
            }
        } else if wildcard(pattern, alias) {
            positive = true;
        }
    }
    positive
}

/// How deep `Include` can go, as in OpenSSH.
const MAX_INCLUDE_DEPTH: usize = 16;

/// One line of the config, with the files it includes already in place.
enum Line {
    /// `where` tells the file and line, for warnings.
    Option {
        key: String,
        values: Vec<String>,
        raw: String,
        place: String,
    },
    /// An included file starts: what it does to the current block ends with it.
    IncludeStart,
    IncludeEnd,
}

/// Reads the config. `text` is `~/.ssh/config`, and what it includes comes from the disk.
pub(crate) fn parse(text: &str, home: &Path) -> (Vec<HostConfig>, Vec<String>) {
    let mut lines = Vec::new();
    let mut warnings = Vec::new();
    read_lines(text, "ssh config", home, 0, &mut lines, &mut warnings);
    let mut aliases = BTreeSet::new();
    for line in &lines {
        if let Line::Option { key, values, .. } = line
            && key == "host"
        {
            for alias in values {
                if !alias.contains(['*', '?']) && !alias.starts_with('!') {
                    aliases.insert(alias.clone());
                }
            }
        }
    }
    let mut hosts = Vec::new();
    let local_user = HostConfig::default().user;
    for alias in aliases {
        let mut host = HostConfig {
            id: HostId(alias.clone()),
            hostname: alias.clone(),
            alias,
            source: HostSource::SshConfig,
            ..Default::default()
        };
        let mut active = true;
        // Inside an include of a block that does not apply, nothing applies.
        let mut never = false;
        let mut includes = Vec::new();
        let mut seen = HashSet::new();
        for entry in &lines {
            let (key, values, raw, line) = match entry {
                Line::IncludeStart => {
                    includes.push((active, never));
                    never = never || !active;
                    continue;
                }
                Line::IncludeEnd => {
                    (active, never) = includes.pop().unwrap_or((true, false));
                    continue;
                }
                Line::Option {
                    key,
                    values,
                    raw,
                    place,
                } => (key, values, raw, place),
            };
            if key == "host" {
                active = !never && matches(values, &host.alias);
                continue;
            }
            if key == "match" {
                active = !never && match_block(values, &host, &local_user);
                continue;
            }
            if !active || values.is_empty() {
                continue;
            }
            let cumulative = matches!(
                key.as_str(),
                "identityfile" | "localforward" | "remoteforward" | "dynamicforward"
            );
            if !cumulative && !seen.insert(key.clone()) {
                continue;
            }
            // The first of the two wins, as in OpenSSH.
            if (key == "proxyjump" && seen.contains("proxycommand"))
                || (key == "proxycommand" && seen.contains("proxyjump"))
            {
                continue;
            }
            let value = &values[0];
            let result: Result<()> = (|| {
                match key.as_str() {
                    "hostname" => host.hostname = expand(value, &[('h', &host.alias)]),
                    "user" => host.user = value.clone(),
                    "port" => host.port = value.parse().context("Invalid port")?,
                    "identityfile" => {
                        if value != "none" {
                            host.identity_files.push(PathBuf::from(value));
                        }
                    }
                    "proxycommand" => {
                        host.proxy_command = if value.eq_ignore_ascii_case("none") {
                            None
                        } else {
                            Some(if values.len() == 1 && raw.starts_with(['\'', '"']) {
                                value.clone()
                            } else {
                                raw.clone()
                            })
                        }
                    }
                    "proxyjump" => {
                        host.proxy_jump =
                            (!value.eq_ignore_ascii_case("none")).then(|| values.concat())
                    }
                    "forwardagent" => host.forward_agent = value.eq_ignore_ascii_case("yes"),
                    "localforward" => host.local_forwards.push(forward(values)?),
                    "remoteforward" => {
                        let mut spec = forward(values)?;
                        spec.kind = ForwardKind::Remote;
                        host.local_forwards.push(spec);
                    }
                    "dynamicforward" => {
                        let (bind_addr, bind_port) = endpoint(value, Some("127.0.0.1"))?;
                        host.local_forwards
                            .push(ForwardSpec::socks(bind_addr, bind_port));
                    }
                    "serveraliveinterval" => {
                        host.server_alive_interval =
                            Some(value.parse().context("Invalid keepalive interval")?)
                    }
                    "serveralivecountmax" => {
                        host.server_alive_count_max =
                            value.parse().context("Invalid keepalive count")?
                    }
                    _ => tracing::debug!(keyword = key, "Ignoring SSH option"),
                }
                Ok(())
            })();
            if let Err(error) = result {
                warnings.push(format!("{line}: {error:#}"));
            }
        }
        expand_identity_files(&mut host, home);
        hosts.push(host);
    }
    warnings.dedup();
    (hosts, warnings)
}

/// Splits a config into lines, reading what it includes.
fn read_lines(
    text: &str,
    file: &str,
    home: &Path,
    depth: usize,
    lines: &mut Vec<Line>,
    warnings: &mut Vec<String>,
) {
    for (index, line) in text.lines().enumerate() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let place = format!("{file} line {}", index + 1);
        let split = line
            .find(|c: char| c.is_whitespace() || c == '=')
            .unwrap_or(line.len());
        let key = line[..split].to_ascii_lowercase();
        let raw = line[split..]
            .trim_start()
            .trim_start_matches('=')
            .trim_start();
        let values = match words(raw) {
            Ok(values) => values,
            Err(error) => {
                warnings.push(format!("{place}: {error}"));
                continue;
            }
        };
        if key == "include" {
            if depth >= MAX_INCLUDE_DEPTH {
                warnings.push(format!("{place}: Include goes too deep"));
                continue;
            }
            lines.push(Line::IncludeStart);
            for pattern in &values {
                for path in include_paths(pattern, home) {
                    match std::fs::read_to_string(&path) {
                        Ok(text) => {
                            let name = path.display().to_string();
                            read_lines(&text, &name, home, depth + 1, lines, warnings);
                        }
                        Err(error) => {
                            warnings.push(format!("{place}: {}: {error}", path.display()))
                        }
                    }
                }
            }
            lines.push(Line::IncludeEnd);
            continue;
        }
        if key == "match"
            && let Some(criterion) = unsupported_criterion(&values)
        {
            warnings.push(format!(
                "{place}: Match {criterion} is not supported, so this block is skipped"
            ));
        }
        lines.push(Line::Option {
            key,
            values,
            raw: raw.to_string(),
            place,
        });
    }
}

/// The files an `Include` means, in order. A relative path starts at `~/.ssh`, as in OpenSSH, and
/// wildcards can be in any part of it. A pattern that finds nothing is not an error.
fn include_paths(pattern: &str, home: &Path) -> Vec<PathBuf> {
    let (mut roots, rest) = if let Some(rest) = pattern
        .strip_prefix("~/")
        .or_else(|| pattern.strip_prefix("~\\"))
    {
        (vec![home.to_path_buf()], rest)
    } else if Path::new(pattern).is_absolute() {
        let path = Path::new(pattern);
        let root = path.ancestors().last().unwrap_or(path);
        let mut root = root.to_path_buf();
        for component in path.components() {
            use std::path::Component::{Prefix, RootDir};
            match component {
                Prefix(_) | RootDir => root.push(component),
                _ => break,
            }
        }
        let skip = root.to_string_lossy().len();
        (vec![root], pattern.get(skip..).unwrap_or_default())
    } else {
        (vec![home.join(".ssh")], pattern)
    };
    for part in rest.split(['/', '\\']).filter(|part| !part.is_empty()) {
        let mut next = Vec::new();
        for root in &roots {
            if !part.contains(['*', '?']) {
                next.push(root.join(part));
                continue;
            }
            let mut names: Vec<_> = std::fs::read_dir(root)
                .into_iter()
                .flatten()
                .flatten()
                .map(|entry| entry.file_name().to_string_lossy().into_owned())
                .filter(|name| wildcard(part, name))
                .collect();
            names.sort();
            next.extend(names.into_iter().map(|name| root.join(name)));
        }
        roots = next;
    }
    let wild = pattern.contains(['*', '?']);
    roots
        .into_iter()
        // A folder caught by a wildcard is no config; a missing file named outright says nothing.
        .filter(|path| path.is_file() || (!wild && path.exists()))
        .collect()
}

/// Splits `user=root` the way `user root` reads.
fn criteria(values: &[String]) -> Vec<&str> {
    values
        .iter()
        .flat_map(|value| match value.split_once('=') {
            Some((key, rest)) => vec![key, rest],
            None => vec![value.as_str()],
        })
        .collect()
}

/// Criteria that need no argument.
fn is_flag(criterion: &str) -> bool {
    matches!(criterion, "all" | "final" | "canonical")
}

/// The first criterion that cannot be evaluated. `exec` would run a command just to list hosts.
fn unsupported_criterion(values: &[String]) -> Option<String> {
    let tokens = criteria(values);
    let mut tokens = tokens.iter();
    while let Some(token) = tokens.next() {
        let name = token.trim_start_matches('!').to_ascii_lowercase();
        if is_flag(&name) {
            continue;
        }
        if !matches!(
            name.as_str(),
            "host" | "originalhost" | "user" | "localuser"
        ) {
            return Some(name);
        }
        tokens.next();
    }
    None
}

/// Whether a `Match` line applies to a host, with what is known of it so far.
fn match_block(values: &[String], host: &HostConfig, local_user: &str) -> bool {
    if unsupported_criterion(values).is_some() {
        return false;
    }
    let tokens = criteria(values);
    let mut tokens = tokens.iter();
    while let Some(token) = tokens.next() {
        let (negated, name) = match token.strip_prefix('!') {
            Some(name) => (true, name),
            None => (false, *token),
        };
        let name = name.to_ascii_lowercase();
        let result = if is_flag(&name) {
            name != "canonical"
        } else {
            let Some(patterns) = tokens.next() else {
                return false;
            };
            let value = match name.as_str() {
                "host" => &host.hostname,
                "originalhost" => &host.alias,
                "user" => &host.user,
                _ => local_user,
            };
            let patterns: Vec<String> = patterns.split(',').map(str::to_string).collect();
            matches(&patterns, value)
        };
        if result == negated {
            return false;
        }
    }
    true
}

/// The hosts to go through on the way to `host`, in the order to connect to them. A jump host
/// named by its alias brings its own settings, and its own jumps come before it.
pub(crate) fn jump_chain(host: &HostConfig, known: &[HostConfig]) -> Result<Vec<HostConfig>> {
    let mut chain = Vec::new();
    let mut path = vec![host.alias.clone()];
    add_jumps(host, known, &mut path, &mut chain)?;
    Ok(chain)
}
fn add_jumps(
    host: &HostConfig,
    known: &[HostConfig],
    path: &mut Vec<String>,
    chain: &mut Vec<HostConfig>,
) -> Result<()> {
    let Some(list) = &host.proxy_jump else {
        return Ok(());
    };
    for spec in list.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let hop = jump_host(spec, known)?;
        if path.contains(&hop.alias) {
            bail!("ProxyJump goes around in a circle through {}", hop.alias);
        }
        path.push(hop.alias.clone());
        add_jumps(&hop, known, path, chain)?;
        path.pop();
        chain.push(hop);
    }
    Ok(())
}
/// `[ssh://][user@]host[:port]`, where `host` can be an alias.
fn jump_host(spec: &str, known: &[HostConfig]) -> Result<HostConfig> {
    let text = spec.strip_prefix("ssh://").unwrap_or(spec);
    let (user, text) = match text.rsplit_once('@') {
        Some((user, rest)) => (Some(user), rest),
        None => (None, text),
    };
    let (name, port) = if let Some(rest) = text.strip_prefix('[') {
        let (name, rest) = rest.split_once(']').context("Expected [address]")?;
        (name, rest.strip_prefix(':'))
    } else {
        match text.rsplit_once(':') {
            Some((name, port)) => (name, Some(port)),
            None => (text, None),
        }
    };
    if name.is_empty() {
        bail!("ProxyJump \"{spec}\" names no host");
    }
    let mut hop = known
        .iter()
        .find(|host| host.alias == name)
        .cloned()
        .unwrap_or_else(|| HostConfig {
            id: HostId(name.to_string()),
            alias: name.to_string(),
            hostname: name.to_string(),
            source: HostSource::SshConfig,
            ..Default::default()
        });
    if let Some(user) = user {
        hop.user = user.to_string();
    }
    if let Some(port) = port {
        hop.port = port
            .parse()
            .with_context(|| format!("Bad port in ProxyJump \"{spec}\""))?;
    }
    Ok(hop)
}

pub(crate) fn expand_identity_files(host: &mut HostConfig, home: &Path) {
    let local_user = HostConfig::default().user;
    host.identity_files = host
        .identity_files
        .iter()
        .map(|p| {
            let text = expand(
                &p.to_string_lossy(),
                &[
                    ('d', home.to_string_lossy().as_ref()),
                    ('u', &local_user),
                    ('h', &host.hostname),
                    ('r', &host.user),
                ],
            );
            if text == "~" {
                home.to_path_buf()
            } else if text.starts_with("~/") || text.starts_with("~\\") {
                home.join(&text[2..])
            } else {
                PathBuf::from(text)
            }
        })
        .collect();
}

pub(crate) fn expand(text: &str, tokens: &[(char, &str)]) -> String {
    let mut out = String::new();
    let mut chars = text.chars();
    while let Some(ch) = chars.next() {
        if ch != '%' {
            out.push(ch);
            continue;
        }
        match chars.next() {
            Some('%') => out.push('%'),
            Some(token) => {
                if let Some((_, value)) = tokens.iter().find(|(k, _)| *k == token) {
                    out.push_str(value);
                } else {
                    out.push('%');
                    out.push(token);
                }
            }
            None => out.push('%'),
        }
    }
    out
}
fn endpoint(value: &str, default_host: Option<&str>) -> Result<(String, u16)> {
    if let Some(rest) = value.strip_prefix('[') {
        let (host, port) = rest.split_once("]:").context("Expected [address]:port")?;
        return Ok((host.into(), port.parse().context("Invalid forward port")?));
    }
    if let Some((host, port)) = value.rsplit_once(':') {
        return Ok((
            if host.is_empty() { "127.0.0.1" } else { host }.into(),
            port.parse().context("Invalid forward port")?,
        ));
    }
    Ok((
        default_host.context("Expected host:port")?.into(),
        value.parse().context("Invalid forward port")?,
    ))
}
fn forward(values: &[String]) -> Result<ForwardSpec> {
    if values.len() != 2 {
        bail!("LocalForward requires a listener and destination");
    }
    let (bind_addr, bind_port) = endpoint(&values[0], Some("127.0.0.1"))?;
    let (remote_host, remote_port) = endpoint(&values[1], None)?;
    Ok(ForwardSpec {
        bind_addr,
        bind_port,
        remote_host,
        remote_port,
        kind: ForwardKind::Local,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    struct Home(PathBuf);
    impl Home {
        fn new(name: &str) -> Self {
            let dir =
                std::env::temp_dir().join(format!("shcp-ssh-config-{name}-{}", std::process::id()));
            fs::create_dir_all(dir.join(".ssh")).unwrap();
            Self(dir)
        }
        fn file(&self, relative: &str, text: &str) {
            let path = self.0.join(".ssh").join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, text).unwrap();
        }
    }
    impl Drop for Home {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn parsed(text: &str) -> (Vec<HostConfig>, Vec<String>) {
        parse(text, Path::new("/nonexistent-home"))
    }
    fn host<'a>(hosts: &'a [HostConfig], alias: &str) -> &'a HostConfig {
        hosts.iter().find(|h| h.alias == alias).unwrap()
    }

    #[test]
    fn plain_options_are_read_and_the_first_value_wins() {
        let (hosts, warnings) = parsed(
            "Host web\n  HostName 10.0.0.1\n  User first\n  Port 2200\n\
             Host *\n  User second\n  ServerAliveInterval 15\n",
        );
        let web = host(&hosts, "web");
        assert_eq!(web.hostname, "10.0.0.1");
        assert_eq!(web.user, "first");
        assert_eq!(web.port, 2200);
        assert_eq!(web.server_alive_interval, Some(15));
        assert!(warnings.is_empty());
    }

    #[test]
    fn include_brings_hosts_from_other_files_in_name_order() {
        let home = Home::new("include");
        home.file("config.d/b", "Host one\n  User from-b\n");
        home.file("config.d/a", "Host one\n  User from-a\nHost two\n");
        let (hosts, warnings) = parse("Include config.d/*\n", &home.0);
        assert_eq!(host(&hosts, "one").user, "from-a");
        assert_eq!(hosts.len(), 2);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn include_with_a_tilde_and_a_missing_file_are_fine() {
        let home = Home::new("tilde");
        home.file("extra", "Host three\n");
        let (hosts, warnings) = parse("Include ~/.ssh/extra ~/.ssh/missing\n", &home.0);
        assert_eq!(hosts.len(), 1);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn an_include_inside_a_block_applies_only_to_that_block() {
        let home = Home::new("block");
        home.file("common", "User shared\nHost *\nPort 2222\n");
        let (hosts, _) = parse(
            "Host inside\n  Include common\nHost outside\n  HostName elsewhere\n",
            &home.0,
        );
        assert_eq!(host(&hosts, "inside").user, "shared");
        assert_eq!(host(&hosts, "inside").port, 2222);
        assert_eq!(host(&hosts, "outside").port, 22);
        assert_ne!(host(&hosts, "outside").user, "shared");
    }

    #[test]
    fn a_file_including_itself_stops_with_a_warning() {
        let home = Home::new("loop");
        home.file("self", "Host looped\nInclude self\n");
        let (hosts, warnings) = parse("Include self\n", &home.0);
        assert_eq!(hosts.len(), 1);
        assert!(
            warnings.iter().any(|w| w.contains("too deep")),
            "{warnings:?}"
        );
    }

    #[test]
    fn match_host_user_and_negation_are_evaluated() {
        let (hosts, warnings) = parsed(
            "Host prod\n  HostName app.prod.example\n  User deploy\n\
             Host dev\n  HostName app.dev.example\n\
             Match host *.prod.example user deploy\n  Port 2201\n\
             Match !host *.prod.example\n  Port 2202\n\
             Match originalhost dev,other\n  IdentityFile ~/dev-key\n",
        );
        assert_eq!(host(&hosts, "prod").port, 2201);
        assert_eq!(host(&hosts, "dev").port, 2202);
        assert!(host(&hosts, "prod").identity_files.is_empty());
        assert_eq!(host(&hosts, "dev").identity_files.len(), 1);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn match_all_and_equals_signs_work() {
        let (hosts, _) = parsed("Host a\nMatch user=nobody\n  Port 1\nMatch all\n  Port 2\n");
        assert_eq!(host(&hosts, "a").port, 2);
    }

    #[test]
    fn match_exec_is_never_run_and_says_so() {
        let (hosts, warnings) = parsed("Host a\nMatch exec \"exit 0\"\n  Port 1\n");
        assert_eq!(host(&hosts, "a").port, 22);
        assert!(warnings[0].contains("exec"), "{warnings:?}");
    }

    #[test]
    fn hostname_can_use_the_alias() {
        let (hosts, _) = parsed("Host box\n  HostName %h.example.com\n");
        assert_eq!(host(&hosts, "box").hostname, "box.example.com");
    }

    #[test]
    fn dynamic_forward_is_a_socks_proxy() {
        let (hosts, warnings) = parsed(
            "Host a\n  DynamicForward 1080\n  DynamicForward 0.0.0.0:1081\n  LocalForward 80 web:80\n",
        );
        let forwards = &host(&hosts, "a").local_forwards;
        assert_eq!(forwards[0], ForwardSpec::socks("127.0.0.1".into(), 1080));
        assert_eq!(forwards[1], ForwardSpec::socks("0.0.0.0".into(), 1081));
        assert_eq!(forwards[2].kind, ForwardKind::Local);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn remote_forward_and_forward_agent_are_read() {
        let (hosts, warnings) = parsed(
            "Host a\n  RemoteForward 9000 localhost:3000\n  ForwardAgent yes\nHost b\n  ForwardAgent no\n",
        );
        let spec = &host(&hosts, "a").local_forwards[0];
        assert_eq!(spec.kind, ForwardKind::Remote);
        assert_eq!(spec.bind_port, 9000);
        assert_eq!(
            (spec.remote_host.as_str(), spec.remote_port),
            ("localhost", 3000)
        );
        assert!(host(&hosts, "a").forward_agent);
        assert!(!host(&hosts, "b").forward_agent);
        assert!(warnings.is_empty(), "{warnings:?}");
    }

    #[test]
    fn the_first_of_proxy_jump_and_proxy_command_wins() {
        let (hosts, _) = parsed(
            "Host a\n  ProxyJump bastion\n  ProxyCommand nc %h %p\n\
             Host b\n  ProxyCommand nc %h %p\n  ProxyJump bastion\n",
        );
        assert_eq!(host(&hosts, "a").proxy_jump.as_deref(), Some("bastion"));
        assert!(host(&hosts, "a").proxy_command.is_none());
        assert!(host(&hosts, "b").proxy_jump.is_none());
        assert!(host(&hosts, "b").proxy_command.is_some());
    }

    #[test]
    fn a_jump_chain_uses_known_hosts_and_comes_in_connection_order() {
        let (hosts, _) = parsed(
            "Host target\n  ProxyJump mid,admin@10.0.0.9:2222\n\
             Host mid\n  HostName mid.example\n  User me\n  ProxyJump edge\n\
             Host edge\n  HostName edge.example\n",
        );
        let chain = jump_chain(host(&hosts, "target"), &hosts).unwrap();
        let names: Vec<_> = chain.iter().map(|h| h.hostname.as_str()).collect();
        assert_eq!(names, ["edge.example", "mid.example", "10.0.0.9"]);
        assert_eq!(chain[1].user, "me");
        assert_eq!(chain[2].user, "admin");
        assert_eq!(chain[2].port, 2222);
    }

    #[test]
    fn a_jump_loop_is_an_error() {
        let (hosts, _) = parsed("Host a\n  ProxyJump b\nHost b\n  ProxyJump a\n");
        assert!(jump_chain(host(&hosts, "a"), &hosts).is_err());
    }

    #[test]
    fn a_jump_address_can_be_ipv6() {
        let hop = jump_host("root@[::1]:2200", &[]).unwrap();
        assert_eq!((hop.hostname.as_str(), hop.port), ("::1", 2200));
    }
}
