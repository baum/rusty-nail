use crate::{telemetry, Error, Result};
use chrono::prelude::*;
use futures::{future::BoxFuture, FutureExt, StreamExt};
use k8s_openapi::api::core::v1::ObjectReference;
use kube::{
    api::{Api, ListParams, Patch, PatchParams, ResourceExt},
    client::Client,
    runtime::{
        controller::{Context, Controller, ReconcilerAction},
        events::{Event, EventType, Recorder, Reporter},
    },
    CustomResource, Resource,
};
use prometheus::{
    default_registry, proto::MetricFamily, register_histogram_vec, register_int_counter, HistogramOpts,
    HistogramVec, IntCounter,
};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{collections::HashMap, sync::Arc};
use tokio::{
    sync::RwLock,
    time::{Duration, Instant},
};
use tracing::{debug, error, event, field, info, instrument, trace, warn, Level, Span};

/// Source defines the event source
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct Source {
    // URL of the NooNaa management RPC service
    #[serde(rename = "rpcUrl")]
    rpc_url: String,

    // cecret name containing credentials for the NooNaa management RPC service
    #[serde(rename = "rpcSecret")]
    rpc_secret: String,

    // Bucket name
    bucket: String,
}
// KReference contains enough information to refer to Sink
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]

pub struct KReference {
    // kind of the referent.
    // More info: https://git.k8s.io/community/contributors/devel/sig-architecture/api-conventions.md#types-kinds
    kind: String,

    // namespace of the referent.
    // More info: https://kubernetes.io/docs/concepts/overview/working-with-objects/namespaces/
    // This is optional field, it gets defaulted to the object holding it if left out.
    namespace: Option<String>,

    // name of the referent.
    // More info: https://kubernetes.io/docs/concepts/overview/working-with-objects/names/#names
    name: String,

    // api version of the referent.
    #[serde(rename = "apiVersion")]
    api_version: Option<String>,

    // group of the API, without the version of the group. This can be used as an alternative to the APIVersion, and then resolved using ResolveGroup.
    // Note: This API is EXPERIMENTAL and might break anytime. For more details: https://github.com/knative/eventing/issues/5086
    group: Option<String>,
}

// Destination represents a target of an invocation over HTTP.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct Destination {
    // ref points to an Addressable.
    #[serde(rename = "ref")]
    reference: Option<KReference>,

    // uri can be an absolute URL(non-empty scheme and non-empty host) pointing to the target or a relative URI. Relative URIs will be resolved using the base URI retrieved from Ref.
    uri: Option<String>,
}

// CloudEventOverrides defines arguments for a Source that control the output
// format of the CloudEvents produced by the Source.
#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct CloudEventOverrides {
    // extensions specify what attribute are added or overridden on the
    // outbound event. Each `Extensions` key-value pair are set on the event as
    // an attribute extension independently.
    extensions: HashMap<String, String>, // map[string]string `json:"extensions,omitempty"`
}

/// NooBaa Knative Source custom resource spec
#[derive(CustomResource, Deserialize, Serialize, Clone, Debug, JsonSchema)]
#[kube(kind = "NooBaaSource", group = "knative.dev", version = "v1", namespaced)]
#[kube(status = "NooBaaSourceStatus")]
pub struct NooBaaSourceSpec {
    name: String,
    source: Source,
    sink: Destination,
    #[serde(rename = "ceOverrides")]
    ce_overrides: Option<CloudEventOverrides>,
}

#[derive(Deserialize, Serialize, Clone, Debug, JsonSchema)]
pub struct NooBaaSourceStatus {
    is_bad: bool,
    //last_updated: Option<DateTime<Utc>>,
}

// Context for our reconciler
#[derive(Clone)]
struct Data {
    /// kubernetes client
    client: Client,
    /// In memory state
    state: Arc<RwLock<State>>,
    /// Various prometheus metrics
    metrics: Metrics,
}

#[instrument(skip(ctx), fields(trace_id))]
async fn reconcile(noobaa_source: Arc<NooBaaSource>, ctx: Context<Data>) -> Result<ReconcilerAction, Error> {
    let trace_id = telemetry::get_trace_id();
    Span::current().record("trace_id", &field::display(&trace_id));
    let start = Instant::now();

    let client = ctx.get_ref().client.clone();
    ctx.get_ref().state.write().await.last_event = Utc::now();
    let reporter = ctx.get_ref().state.read().await.reporter.clone();
    let recorder = Recorder::new(client.clone(), reporter, noobaa_source.object_ref(&()));
    let name = ResourceExt::name(noobaa_source.as_ref());
    let ns = ResourceExt::namespace(noobaa_source.as_ref()).expect("NooBaaSource is namespaced");
    let noobaa_sources: Api<NooBaaSource> = Api::namespaced(client, &ns);

    let new_status = Patch::Apply(json!({
        "apiVersion": "knative.dev/v1",
        "kind": "NooBaaSource",
        "status": NooBaaSourceStatus {
            is_bad: noobaa_source.spec.name.contains("bad"),
            //last_updated: Some(Utc::now()),
        }
    }));
    info!("Reconciling NooBaaSource\"{}\" in {} new status {:?}", name, ns, new_status);
    let ps = PatchParams::apply("cntrlr").force();
    info!("Reconciling NooBaaSource\"{}\" in {} patch params {:?}", name, ns, ps);
    /*
    let _o = noobaa_sources
        .patch_status(&name, &ps, &new_status)
        .await
        .map_err(Error::KubeError)?;
    */
    let patch_result = noobaa_sources
        .patch_status(&name, &ps, &new_status)
        .await;
    info!("Reconciling NooBaaSource\"{}\" in {} patch patch results {:?}", name, ns, patch_result);
    patch_result.map_err(Error::KubeError)?;

    if noobaa_source.spec.name.contains("bad") {
        info!("Reconciling NooBaaSource\"{}\" in {} handle bad in name", name, ns);

        recorder
            .publish(Event {
                type_: EventType::Normal,
                reason: "BadNooBaaSource".into(),
                note: Some(format!("Sending `{}` to detention", name)),
                action: "Correcting".into(),
                secondary: None,
            })
            .await
            .map_err(Error::KubeError)?;
    }

    let duration = start.elapsed().as_millis() as f64 / 1000.0;
    //let ex = Exemplar::new_with_labels(duration, HashMap::from([("trace_id".to_string(), trace_id)]);
    ctx.get_ref()
        .metrics
        .reconcile_duration
        .with_label_values(&[])
        .observe(duration);
    //.observe_with_exemplar(duration, ex);
    ctx.get_ref().metrics.handled_events.inc();
    info!("Reconciled NooBaaSource \"{}\" in {}", name, ns);

    // If no events were received, check back every 30 minutes
    Ok(ReconcilerAction {
        requeue_after: Some(Duration::from_secs(3600 / 2)),
    })
}
fn error_policy(error: &Error, _ctx: Context<Data>) -> ReconcilerAction {
    warn!("reconcile failed: {:?}", error);
    ReconcilerAction {
        requeue_after: Some(Duration::from_secs(360)),
    }
}

/// Metrics exposed on /metrics
#[derive(Clone)]
pub struct Metrics {
    pub handled_events: IntCounter,
    pub reconcile_duration: HistogramVec,
}
impl Metrics {
    fn new() -> Self {
        let reconcile_histogram = register_histogram_vec!(
            "noobaa_source_controller_reconcile_duration_seconds",
            "The duration of reconcile to complete in seconds",
            &[],
            vec![0.01, 0.1, 0.25, 0.5, 1., 5., 15., 60.]
        )
        .unwrap();

        Metrics {
            handled_events: register_int_counter!("noobaa_source_controller_handled_events", "handled events").unwrap(),
            reconcile_duration: reconcile_histogram,
        }
    }
}

/// In-memory reconciler state exposed on /
#[derive(Clone, Serialize)]
pub struct State {
    #[serde(deserialize_with = "from_ts")]
    pub last_event: DateTime<Utc>,
    #[serde(skip)]
    pub reporter: Reporter,
}
impl State {
    fn new() -> Self {
        State {
            last_event: Utc::now(),
            reporter: "noobaa-source-controller".into(),
        }
    }
}

/// Data owned by the Manager
#[derive(Clone)]
pub struct Manager {
    /// In memory state
    state: Arc<RwLock<State>>,
}

/// Example Manager that owns a Controller for NooBaaSource
impl Manager {
    /// Lifecycle initialization interface for app
    ///
    /// This returns a `Manager` that drives a `Controller` + a future to be awaited
    /// It is up to `main` to wait for the controller stream.
    pub async fn new() -> (Self, BoxFuture<'static, ()>) {
        let client = Client::try_default().await.expect("create client");
        let metrics = Metrics::new();
        let state = Arc::new(RwLock::new(State::new()));
        let context = Context::new(Data {
            client: client.clone(),
            metrics: metrics.clone(),
            state: state.clone(),
        });

        let noobaa_sources = Api::<NooBaaSource>::all(client);
        // Ensure CRD is installed before loop-watching
        let _r = noobaa_sources
            .list(&ListParams::default().limit(1))
            .await
            .expect("is the crd installed? please run: cargo run --bin crdgen | kubectl apply -f -");

        // All good. Start controller and return its future.
        let drainer = Controller::new(noobaa_sources, ListParams::default())
            .run(reconcile, error_policy, context)
            .filter_map(|x| async move { std::result::Result::ok(x) })
            .for_each(|_| futures::future::ready(()))
            .boxed();

        (Self { state }, drainer)
    }

    /// Metrics getter
    pub fn metrics(&self) -> Vec<MetricFamily> {
        default_registry().gather()
    }

    /// State getter
    pub async fn state(&self) -> State {
        self.state.read().await.clone()
    }
}
