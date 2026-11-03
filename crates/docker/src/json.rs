//! `sb docker ps --json` (docker spec, decision 2): the rows as one line of
//! compact JSON, keys in a fixed order.

use serde::Serialize;

use crate::ps::Row;

/// One container, in the documented key order (serde keeps the fields'
/// order). Fields are only ever added (scripts depend on them).
#[derive(Serialize)]
struct Container<'a> {
    name: &'a str,
    id: &'a str,
    state: &'a str,
    health: Option<&'a str>,
    exit_code: i64,
    oom_killed: bool,
    status: &'a str,
    role: &'static str,
    traefik_ports: &'a [String],
    bindings: Vec<Binding<'a>>,
}

/// One binding: the address as Docker gives it, ports as numbers (`null`
/// for one that isn't a number).
#[derive(Serialize)]
struct Binding<'a> {
    host_ip: &'a str,
    host_port: Option<u16>,
    container_port: Option<u16>,
    protocol: &'a str,
}

/// The rows as the spec's JSON: compact, one trailing newline, as `sb
/// version --json`.
pub fn json(rows: &[Row]) -> String {
    let containers: Vec<Container<'_>> = rows
        .iter()
        .map(|row| Container {
            name: &row.name,
            id: &row.id,
            state: &row.state,
            health: row.health.as_deref(),
            exit_code: row.exit_code,
            oom_killed: row.oom_killed,
            status: &row.status,
            role: row.role.name(),
            traefik_ports: &row.traefik,
            bindings: row
                .bindings
                .iter()
                .map(|b| Binding {
                    host_ip: &b.host_ip,
                    host_port: b.host_port.parse().ok(),
                    container_port: b.container_port.parse().ok(),
                    protocol: &b.protocol,
                })
                .collect(),
        })
        .collect();
    // Serialising strings, numbers and booleans can't fail.
    let mut text = serde_json::to_string(&containers).unwrap_or_default();
    text.push('\n');
    text
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ps::tests::fixture_rows;

    /// The documented shape, exactly: key order, `null` health without a
    /// health check, the short ID for an unnamed container, bindings with
    /// numeric ports and the address as Docker gives it.
    #[test]
    fn rows_print_the_documented_shape() {
        let text = json(&fixture_rows());
        assert!(text.ends_with("]\n") && !text.contains('\x1b'), "{text}");
        let expected = concat!(
            r#"[{"name":"0123456789ab","id":"0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef","#,
            r#""state":"created","health":null,"exit_code":128,"oom_killed":false,"#,
            r#""status":"created (failed to start)","role":"problem","traefik_ports":[],"bindings":[]},"#,
            r#"{"name":"authelia","id":"c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4","#,
            r#""state":"running","health":null,"exit_code":0,"oom_killed":false,"#,
            r#""status":"running","role":"ok","traefik_ports":["9091"],"#,
            r#""bindings":[{"host_ip":"198.51.100.7","host_port":9091,"container_port":9091,"protocol":"tcp"}]},"#,
            r#"{"name":"lidarr","id":"e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6","#,
            r#""state":"","health":null,"exit_code":0,"oom_killed":false,"#,
            r#""status":"unknown","role":"warn","traefik_ports":[],"bindings":[]},"#,
            r#"{"name":"plex","id":"a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2","#,
            r#""state":"running","health":"healthy","exit_code":0,"oom_killed":false,"#,
            r#""status":"running (healthy)","role":"ok","traefik_ports":["32400"],"#,
            r#""bindings":[{"host_ip":"0.0.0.0","host_port":32400,"container_port":32400,"protocol":"tcp"},"#,
            r#"{"host_ip":"::","host_port":32400,"container_port":32400,"protocol":"tcp"}]},"#,
            r#"{"name":"radarr","id":"d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5","#,
            r#""state":"running","health":null,"exit_code":0,"oom_killed":false,"#,
            r#""status":"running","role":"ok","traefik_ports":[],"#,
            r#""bindings":[{"host_ip":"0.0.0.0","host_port":7878,"container_port":7878,"protocol":"tcp"}]},"#,
            r#"{"name":"sonarr","id":"b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3d4e5f6a1b2c3","#,
            r#""state":"exited","health":null,"exit_code":1,"oom_killed":false,"#,
            r#""status":"exited (code 1)","role":"problem","traefik_ports":["8989"],"bindings":[]}]"#,
            "\n"
        );
        assert_eq!(text, expected);
    }

    #[test]
    fn no_containers_print_an_empty_array() {
        assert_eq!(json(&[]), "[]\n");
    }

    /// A port that isn't a number (never seen from Docker) is `null`.
    #[test]
    fn a_port_that_is_not_a_number_is_null() {
        let mut rows = fixture_rows();
        rows.truncate(2);
        rows[1].bindings[0].host_port = "web".into();
        assert!(json(&rows).contains(r#""host_port":null,"container_port":9091"#));
        rows[1].bindings[0].container_port = "web".into();
        assert!(json(&rows).contains(r#""host_port":null,"container_port":null"#));
    }
}
