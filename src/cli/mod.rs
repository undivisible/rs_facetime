use std::io::{self, Write};

use clap::{Parser, Subcommand};
use serde::Serialize;

use rs_facetime::error::Result;
use rs_facetime::private_api::{
    current_sip_status, BridgeClient, BridgeResponse, Launcher, SipStatus,
};

#[derive(Parser, Debug)]
#[command(
    name = "rs_facetime",
    about = "Unstable — FaceTime Audio bridge CLI for macOS"
)]
pub struct Cli {
    #[arg(long, global = true, help = "Emit one JSON object per line on stdout")]
    pub json: bool,

    #[command(subcommand)]
    pub command: Commands,
}

#[derive(Subcommand, Debug)]
pub enum Commands {
    /// Print SIP status (no bridge required)
    Sip,
    /// Check whether the bridge ready lock exists (no launch)
    Ready,
    /// Launch FaceTime.app with the helper dylib injected
    Connect,
    /// Bridge ping
    Ping,
    /// Bridge status (active call count, bundle id)
    Status,
    /// Start a FaceTime Audio call to a handle (E.164, email, etc.)
    StartCall {
        handle: String,
    },
    /// End all active calls
    EndCall,
    /// Answer a waiting call by UUID
    AnswerCall {
        #[arg(long)]
        call_uuid: String,
    },
    /// Leave/disconnect a call by UUID
    LeaveCall {
        #[arg(long)]
        call_uuid: String,
    },
}

pub fn run(cli: Cli) -> Result<()> {
    match cli.command {
        Commands::Sip => {
            let status = match current_sip_status() {
                SipStatus::Disabled => "disabled",
                SipStatus::Enabled => "enabled",
                SipStatus::Unknown => "unknown",
            };
            emit(cli.json, &serde_json::json!({ "sip": status }))?;
        }
        Commands::Ready => {
            let dylib = Launcher::discover().ok();
            emit(
                cli.json,
                &serde_json::json!({
                    "ready": BridgeClient::is_ready(),
                    "dylib": dylib.as_ref().map(|l| l.dylib_path.display().to_string()),
                }),
            )?;
        }
        Commands::Connect => {
            let launcher = Launcher::discover()?;
            launcher.ensure_launched()?;
            emit(
                cli.json,
                &serde_json::json!({
                    "connected": true,
                    "ready": BridgeClient::is_ready(),
                }),
            )?;
        }
        other => {
            let bridge = BridgeClient::connect()?;
            let response = match other {
                Commands::Ping => bridge.ping()?,
                Commands::Status => bridge.status()?,
                Commands::StartCall { handle } => bridge.start_call(&handle)?,
                Commands::EndCall => bridge.end_call()?,
                Commands::AnswerCall { call_uuid } => bridge.answer_call(&call_uuid)?,
                Commands::LeaveCall { call_uuid } => bridge.leave_call(&call_uuid)?,
                Commands::Sip
                | Commands::Ready
                | Commands::Connect => unreachable!(),
            };
            emit(cli.json, &response_to_json(&response))?;
        }
    }
    Ok(())
}

fn response_to_json(r: &BridgeResponse) -> serde_json::Value {
    serde_json::json!({
        "id": r.id,
        "success": r.success,
        "data": r.data,
        "error": r.error,
    })
}

fn emit<T: Serialize>(json: bool, value: &T) -> Result<()> {
    if json {
        serde_json::to_writer(io::stdout(), value)?;
        writeln!(io::stdout())?;
    } else {
        writeln!(io::stdout(), "{}", serde_json::to_string_pretty(value)?)?;
    }
    Ok(())
}
