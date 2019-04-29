use log::{info, warn, error, debug, trace};
use kubernetes::{
    client::APIClient,
    config::Configuration,
    api::{Named, Cache, Reflector, ApiResource},
};
use std::{
    env,
    time::Duration,
};
use crate::*;

/// Approximation of the CRD we want to work with
/// Replace with own struct.
/// Add serialize for returnability.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct FooResource {
  name: String,
  info: String,
}
impl Named for FooResource {
    // we want Foo identified by self.name in the cache
    fn name(&self) -> String {
        self.name.clone()
    }
}

/// User state for Actix
#[derive(Clone)]
pub struct State {
    // Add resources you need in here, expose it as you see fit
    // this example encapsulates it behind a getter and internal poll thread below.
    foos: Reflector<FooResource>,
}

/// Example state machine that exposes the state of one `Reflector<FooResource>`
///
/// This only deals with a single CRD, and it takes the NAMESPACE from an evar.
impl State {
    fn new(client: APIClient) -> Result<Self> {
        let namespace = env::var("NAMESPACE").expect("Need NAMESPACE evar");
        let fooresource = ApiResource {
            group: "clux.dev".into(),
            resource: "foos".into(),
            namespace: namespace,
        };
        let foos = Reflector::new(client, fooresource)?;
        Ok(State { foos })
    }

    /// Internal poll for internal thread
    fn poll(&self) -> Result<()> {
        self.foos.poll()
    }

    /// Exposed refresh button for use by app
    pub fn refresh(&self) -> Result<()> {
        self.foos.refresh()
    }

    /// Exposed getter for read access to state for app
    pub fn foos(&self) -> Result<Cache<FooResource>> {
        self.foos.read()
    }
}

/// Lifecycle initialization interface for app
///
/// This returns a `State` and calls `poll` on it continuously.
/// As a result, this file encapsulates the only write access to a
pub fn init(cfg: Configuration) -> Result<State> {
    let client = APIClient::new(cfg);
    let state = State::new(client)?; // for app to read
    let state2 = state.clone(); // for internal thread to poll and update
    std::thread::spawn(move || {
        loop {
            std::thread::sleep(Duration::from_secs(10));
            // poll all reflectors here
            // (this can cause a few more waits in edge cases)
            match state2.poll() {
                Ok(_) => trace!("State refreshed"),
                Err(e) => {
                    // Bad fallback, but at least it leaves system working.
                    error!("Failed to refesh cache '{}' - rebooting", e);
                    std::process::exit(1);
                }
            }
        }
    });
    Ok(state)
}