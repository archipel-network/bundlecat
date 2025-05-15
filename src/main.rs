use std::{io::{stdin, stdout, Read, Write}, os::unix::net::UnixStream, path::PathBuf};
use ud3tn_aap::{AapStream, Agent, BaseAgent, RegisteredAgent};
use clap::{CommandFactory, Parser};

#[derive(Debug, Parser)]
#[command(version, about = "Send and Receive bundles with archipel/ud3tn", long_about = None)]
struct Cli {
    /// Print connection information on stderr
    #[arg(short, long)]
    verbose: bool,

    /// Wait for a bundle to be received and output its content on stdout
    #[arg(short, long)]
    listen: bool,

    // When sending bundle, name of source endpoint id to send bundle from
    //
    // Formatted as dtn://<node_id>/<agent_id>
    // 
    // [default: a randomly generated uuid]
    #[arg(short = 'S', long = "source-endpoint")]
    source_endpoint_id: Option<PartialEndpointId>,

    /// Endpoint ID to send to or receive from bundle
    /// 
    /// Should be a full Endpoint ID formatted as dtn://<node_id>/<agent_id>.
    /// Or, when receiving bundles, a simple agent id string that will be
    /// appended to current singleton node ID configured on your node implementation
    /// If ommited when receiving bundle, a random uuid will be generated.
    #[arg(required_unless_present("listen"))]
    endpoint_id: Option<PartialEndpointId>,

    /// Archipel/ud3tn AAP (version 1) socket
    #[arg(long, default_value = "/run/archipel-core/archipel-core.socket")]
    node_sock: PathBuf,
}

#[derive(Debug, Clone, Default)]
struct PartialEndpointId(String);

impl PartialEndpointId {
    fn split_node_agent(&self) -> (Option<&str>, Option<&str>) {
        if self.0.starts_with("dtn://") {
            let Some((node_id, agent_id)) = self.0[6..].split_once("/") else {
                return (Some(&self.0), None);
            };

            if agent_id.is_empty() {
                return (Some(&self.0), None)
            } else {
                return (Some(&self.0[0..(6+node_id.len()+1)]), Some(agent_id));
            }

        } else if !self.0.is_empty() {
            return (None, Some(&self.0))
        } else {
            return (None, None)
        }
    }

    pub fn node_id(&self) -> Option<&str> {
        return self.split_node_agent().0
    }

    pub fn agent_id(&self) -> Option<&str> {
        return self.split_node_agent().1
    }

    pub fn is_singleton_node(&self) -> bool {
        match self.node_id().and_then(|it| it.chars().skip(6).next()) {
            Some('~') => false,
            None | Some(_) => true,
        }
    }
}

impl From<String> for PartialEndpointId {
    fn from(value: String) -> Self {
        Self(value)
    }
}

macro_rules! log {
    ($verbose: expr, $($arg:tt)*) => {{
        if $verbose { eprintln!($($arg)*); }
    }};
}

fn main() {
    let cli = Cli::parse();
    let verbose = cli.verbose;

    if ! cli.listen {
        let mut cmd = Cli::command();
        if cli.endpoint_id.as_ref().is_none_or(|it| it.node_id().is_none()) {
            cmd.error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "When sending a bundle, destination eid must contains a node_id part".to_string())
                .exit();
        } else if cli.endpoint_id.as_ref().is_none_or(|it| it.agent_id().is_none()) {
            cmd.error(
                clap::error::ErrorKind::MissingRequiredArgument,
                "When sending a bundle, destination eid must contains a agent_id part".to_string())
                .exit();
        } else if cli.source_endpoint_id.as_ref().is_some_and(|it| !it.is_singleton_node()) {
            eprintln!("Sending bundle from non-singleton node_id is not supported");
            std::process::exit(1);
        }
    } else if cli.endpoint_id.as_ref().is_some_and(|it| !it.is_singleton_node()) {
        eprintln!("Listening on non-singleton node_id is not supported");
        std::process::exit(1);
    }

    let unix_stream = match UnixStream::connect(cli.node_sock.clone()) {
        Err(e) => {
            eprintln!("Failed to connect to node socket: {e}");
            std::process::exit(10);
        },
        Ok(s) => s
    };

    log!(verbose, "Connected to node on {}", cli.node_sock.to_string_lossy());

    let agent = match Agent::new(unix_stream) {
        Err(e) => {
            eprint!("Failed to establish a connection with node: {e}");
            std::process::exit(11);
        },
        Ok(a) => {
            log!(verbose, "Welcome from node {}", a.node_id());
            a
        }
    };

    if (cli.listen && 
            cli.endpoint_id.as_ref().is_some_and(
                |it| it.node_id().is_some_and(|it| it != agent.node_id()))) ||
        cli.source_endpoint_id.as_ref().is_some_and(
            |it| it.node_id()
    .is_some_and(|it| it != agent.node_id())) {
        eprintln!("Provided node id is different from node id configured on server ({})", agent.node_id());
        std::process::exit(2);
    }

    let agent_id = (if cli.listen { 
        cli.endpoint_id.clone()
    } else {
        cli.source_endpoint_id.clone()
    }).unwrap_or_default()
    .agent_id().map(|it| it.to_owned())
    .unwrap_or_else(|| uuid::Uuid::new_v4().to_string() );

    let agent = match agent.register(agent_id.clone()) {
        Err(e) => {
            eprint!("Failed to establish a connection with node: {e}");
            std::process::exit(11);
        },
        Ok(a) => {
            log!(verbose, "Agent registered on endpoint {}{}", a.node_id(), a.agent_id());
            a
        }
    };


    if cli.listen {
        receive(agent, verbose);
    } else {
        let destination_eid = cli.endpoint_id.unwrap();
        send(agent, verbose, destination_eid);
    }
}

fn send<S: AapStream>(mut agent: RegisteredAgent<S>, verbose: bool, destination: PartialEndpointId){

    let destination_eid = destination.0;

    let mut bundle_content: Vec<u8> = Vec::with_capacity(1024);
    let mut buffer = [0;1024];

    loop {
        let byte_red = match stdin().read(&mut buffer) {
            Ok(result) => result,
            Err(e) => {
                eprintln!("Failed to read from stdin: {e}");
                std::process::exit(13);
            }
        };

        if byte_red > 0 {
            bundle_content.extend_from_slice(&buffer[0..byte_red]);
        } else {
            break;
        }
    }

    let bundle_size = bundle_content.len();

    if let Err(e) = agent.send_bundle(destination_eid.clone(), &bundle_content) {
        eprint!("Failed to send bundle: {e}");
        std::process::exit(14);
    }

    log!(verbose, "Sent {} byte bundle to {}", bundle_size, destination_eid);
}

fn receive<S: AapStream>(mut agent: RegisteredAgent<S>, verbose: bool){
    let mut bundle = match agent.recv_bundle() {
        Ok(b) => b,
        Err(e) => {
            eprintln!("Failed to receive bundle: {e}");
            std::process::exit(15);
        }
    };

    if let Some(source) = bundle.source.as_ref() {
        log!(verbose, "Received bundle from {}", source);
    } else {
        log!(verbose, "Received bundle from unknown source");
    }

    if let Err(e) = stdout().write_all(&mut bundle.payload) {
        eprintln!("Failed to write to stdout: {e}");
        std::process::exit(16);
    }
}