use std::{env, error::Error, ops::Sub, time::Duration};

use axum::{
    async_trait,
    body::{Body, Bytes},
    extract::FromRequest,
    extract::Request,
    routing::post,
    Router,
};
use bytes::Buf;
use capnp::traits::Imbue;
use capnp::{
    message::{ReaderOptions, TypedReader},
    serialize::OwnedSegments,
    serialize_packed,
    traits::Owned,
};
use protocol::measurement::{self, measurement::Which};
use protocol::measurements;
use tokio::{net::TcpListener, time::Instant};

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    tracing_subscriber::fmt::init();

    tracing::info!("HELLO");

    let addr = env::args()
        .nth(1)
        .unwrap_or_else(|| "0.0.0.0:8080".to_string());
    let listener = TcpListener::bind(&addr).await?;
    // build our application with a route

    let app = Router::new()
        // `GET /` goes to `root`
        .route("/", post(root));

    axum::serve(listener, app).await.unwrap();
    Ok(())
}

// basic handler that responds with a static string
async fn root(Capnp(measurements): Capnp<measurements::Owned>) {
    tracing::info!("HERE!");
    let reader = measurements.get().unwrap();
    let measurements = reader.get_measurements().unwrap();
    for measurement in measurements.into_iter() {
        let (sensor, value) = match measurement.get_measurement().which() {
            Ok(Which::Temperature(t)) => ("temperature", format!("{}", t)),
            Ok(Which::Humidity(t)) => ("humidity", format!("{}", t)),
            Ok(Which::Co2(t)) => ("co2", format!("{}", t)),
            Err(_) => continue,
        };
        tracing::info!(
            " got {} {} {} secs ago",
            sensor,
            value,
            measurement.get_time_since()
        );
    }
}

struct Capnp<T: Owned>(TypedReader<OwnedSegments, T>);

#[async_trait]
impl<T: Owned, S: Send + Sync> FromRequest<S> for Capnp<T> {
    type Rejection = <Bytes as FromRequest<S>>::Rejection;

    async fn from_request(
        req: Request<Body>,
        state: &S,
    ) -> Result<Self, <Self as FromRequest<S>>::Rejection> {
        let bytes = Bytes::from_request(req, state).await?;
        let message_reader =
            serialize_packed::read_message(bytes.reader(), ReaderOptions::new()).unwrap();
        let typed_reader = TypedReader::<_, T>::new(message_reader);
        Ok(Capnp(typed_reader))
    }
}
