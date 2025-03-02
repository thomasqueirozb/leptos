#![forbid(unsafe_code)]

//! Provides functions to easily integrate Leptos with Axum.
//!
//! For more details on how to use the integrations, see the
//! [`examples`](https://github.com/leptos-rs/leptos/tree/main/examples)
//! directory in the Leptos repository.

use axum::{
    body::{Body, Bytes, Full, StreamBody},
    extract::Path,
    http::{header::HeaderName, header::HeaderValue, HeaderMap, Request, StatusCode},
    response::IntoResponse,
    routing::get,
};
use futures::{Future, SinkExt, Stream, StreamExt};
use http::{header, method::Method, uri::Uri, version::Version, Response};
use hyper::body;
use leptos::*;
use leptos_meta::MetaContext;
use leptos_router::*;
use std::{io, pin::Pin, sync::Arc};
use tokio::{sync::RwLock, task::spawn_blocking, task::LocalSet};

/// A struct to hold the parts of the incoming Request. Since `http::Request` isn't cloneable, we're forced
/// to construct this for Leptos to use in Axum
#[derive(Debug, Clone)]
pub struct RequestParts {
    pub version: Version,
    pub method: Method,
    pub uri: Uri,
    pub headers: HeaderMap<HeaderValue>,
    pub body: Bytes,
}
/// This struct lets you define headers and override the status of the Response from an Element or a Server Function
/// Typically contained inside of a ResponseOptions. Setting this is useful for cookies and custom responses.
#[derive(Debug, Clone, Default)]
pub struct ResponseParts {
    pub status: Option<StatusCode>,
    pub headers: HeaderMap,
}

impl ResponseParts {
    /// Insert a header, overwriting any previous value with the same key
    pub fn insert_header(&mut self, key: HeaderName, value: HeaderValue) {
        self.headers.insert(key, value);
    }
    /// Append a header, leaving any header with the same key intact
    pub fn append_header(&mut self, key: HeaderName, value: HeaderValue) {
        self.headers.append(key, value);
    }
}

/// Adding this Struct to your Scope inside of a Server Fn or Element will allow you to override details of the Response
/// like status and add Headers/Cookies. Because Elements and Server Fns are lower in the tree than the Response generation
/// code, it needs to be wrapped in an `Arc<RwLock<>>` so that it can be surfaced.
#[derive(Debug, Clone, Default)]
pub struct ResponseOptions(pub Arc<RwLock<ResponseParts>>);

impl ResponseOptions {
    /// A less boilerplatey way to overwrite the contents of `ResponseOptions` with a new `ResponseParts`
    pub async fn overwrite(&self, parts: ResponseParts) {
        let mut writable = self.0.write().await;
        *writable = parts
    }
    /// Set the status of the returned Response
    pub async fn set_status(&self, status: StatusCode) {
        let mut writeable = self.0.write().await;
        let res_parts = &mut *writeable;
        res_parts.status = Some(status);
    }
    /// Insert a header, overwriting any previous value with the same key
    pub async fn insert_header(&self, key: HeaderName, value: HeaderValue) {
        let mut writeable = self.0.write().await;
        let res_parts = &mut *writeable;
        res_parts.headers.insert(key, value);
    }
    /// Append a header, leaving any header with the same key intact
    pub async fn append_header(&self, key: HeaderName, value: HeaderValue) {
        let mut writeable = self.0.write().await;
        let res_parts = &mut *writeable;
        res_parts.headers.append(key, value);
    }
}

/// Provides an easy way to redirect the user from within a server function. Mimicing the Remix `redirect()`,
/// it sets a StatusCode of 302 and a LOCATION header with the provided value.
/// If looking to redirect from the client, `leptos_router::use_navigate()` should be used instead
pub async fn redirect(cx: leptos::Scope, path: &str) {
    let response_options = use_context::<ResponseOptions>(cx).unwrap();
    response_options.set_status(StatusCode::FOUND).await;
    response_options
        .insert_header(
            header::LOCATION,
            header::HeaderValue::from_str(path).expect("Failed to create HeaderValue"),
        )
        .await;
}

/// Decomposes an HTTP request into its parts, allowing you to read its headers
/// and other data without consuming the body.
pub async fn generate_request_parts(req: Request<Body>) -> RequestParts {
    // provide request headers as context in server scope
    let (parts, body) = req.into_parts();
    let body = body::to_bytes(body).await.unwrap_or_default();
    RequestParts {
        method: parts.method,
        uri: parts.uri,
        headers: parts.headers,
        version: parts.version,
        body,
    }
}

/// An Axum handlers to listens for a request with Leptos server function arguments in the body,
/// run the server function if found, and return the resulting [Response].
///
/// This can then be set up at an appropriate route in your application:
///
/// ```
/// use axum::{handler::Handler, routing::post, Router};
/// use std::net::SocketAddr;
/// use leptos::*;
///
/// # if false { // don't actually try to run a server in a doctest...
/// #[tokio::main]
/// async fn main() {
///     let addr = SocketAddr::from(([127, 0, 0, 1], 8082));
///
///     // build our application with a route
///     let app = Router::new()
///       .route("/api/*fn_name", post(leptos_axum::handle_server_fns));
///
///     // run our app with hyper
///     // `axum::Server` is a re-export of `hyper::Server`
///     axum::Server::bind(&addr)
///         .serve(app.into_make_service())
///         .await
///         .unwrap();
/// }
/// # }
/// ```
/// Leptos provides a generic implementation of `handle_server_fns`. If access to more specific parts of the Request is desired,
/// you can specify your own server fn handler based on this one and give it it's own route in the server macro.
///
/// ## Provided Context Types
/// This function always provides context values including the following types:
/// - [RequestParts]
/// - [ResponseOptions]
pub async fn handle_server_fns(
    Path(fn_name): Path<String>,
    headers: HeaderMap,
    req: Request<Body>,
) -> impl IntoResponse {
    handle_server_fns_inner(fn_name, headers, |_| {}, req).await
}

/// An Axum handlers to listens for a request with Leptos server function arguments in the body,
/// run the server function if found, and return the resulting [Response].
///
/// This can then be set up at an appropriate route in your application:
///
/// This version allows you to pass in a closure to capture additional data from the layers above leptos
/// and store it in context. To use it, you'll need to define your own route, and a handler function
/// that takes in the data you'd like. See the [render_app_to_stream_with_context] docs for an example
/// of one that should work much like this one.
///
/// ## Provided Context Types
/// This function always provides context values including the following types:
/// - [RequestParts]
/// - [ResponseOptions]
pub async fn handle_server_fns_with_context(
    Path(fn_name): Path<String>,
    headers: HeaderMap,
    additional_context: impl Fn(leptos::Scope) + 'static + Clone + Send,
    req: Request<Body>,
) -> impl IntoResponse {
    handle_server_fns_inner(fn_name, headers, additional_context, req).await
}

async fn handle_server_fns_inner(
    fn_name: String,
    headers: HeaderMap,
    additional_context: impl Fn(leptos::Scope) + 'static + Clone + Send,
    req: Request<Body>,
) -> impl IntoResponse {
    // Axum Path extractor doesn't remove the first slash from the path, while Actix does
    let fn_name = fn_name
        .strip_prefix('/')
        .map(|fn_name| fn_name.to_string())
        .unwrap_or(fn_name);

    let (tx, rx) = futures::channel::oneshot::channel();
    spawn_blocking({
        move || {
            tokio::runtime::Runtime::new()
                .expect("couldn't spawn runtime")
                .block_on({
                    async move {
                        let res = if let Some(server_fn) = server_fn_by_path(fn_name.as_str()) {
                            let runtime = create_runtime();
                            let (cx, disposer) = raw_scope_and_disposer(runtime);

                            additional_context(cx);

                            let req_parts = generate_request_parts(req).await;
                            // Add this so we can get details about the Request
                            provide_context(cx, req_parts.clone());
                            // Add this so that we can set headers and status of the response
                            provide_context(cx, ResponseOptions::default());

                            match server_fn(cx, &req_parts.body).await {
                                Ok(serialized) => {
                                    // If ResponseOptions are set, add the headers and status to the request
                                    let res_options = use_context::<ResponseOptions>(cx);

                                    // clean up the scope, which we only needed to run the server fn
                                    disposer.dispose();
                                    runtime.dispose();

                                    // if this is Accept: application/json then send a serialized JSON response
                                    let accept_header =
                                        headers.get("Accept").and_then(|value| value.to_str().ok());
                                    let mut res = Response::builder();

                                    // Add headers from ResponseParts if they exist. These should be added as long
                                    // as the server function returns an OK response
                                    let res_options_outer = res_options.unwrap().0;
                                    let res_options_inner = res_options_outer.read().await;
                                    let (status, mut res_headers) = (
                                        res_options_inner.status,
                                        res_options_inner.headers.clone(),
                                    );

                                    if let Some(header_ref) = res.headers_mut() {
                                           header_ref.extend(res_headers.drain());
                                    };

                                    if accept_header == Some("application/json")
                                        || accept_header
                                            == Some("application/x-www-form-urlencoded")
                                        || accept_header == Some("application/cbor")
                                    {
                                        res = res.status(StatusCode::OK);
                                    }
                                    // otherwise, it's probably a <form> submit or something: redirect back to the referrer
                                    else {
                                        let referer = headers
                                            .get("Referer")
                                            .and_then(|value| value.to_str().ok())
                                            .unwrap_or("/");

                                        res = res
                                            .status(StatusCode::SEE_OTHER)
                                            .header("Location", referer);
                                    }
                                    // Override StatusCode if it was set in a Resource or Element
                                    res = match status {
                                        Some(status) => res.status(status),
                                        None => res,
                                    };
                                    match serialized {
                                        Payload::Binary(data) => res
                                            .header("Content-Type", "application/cbor")
                                            .body(Full::from(data)),
                                        Payload::Url(data) => res
                                            .header(
                                                "Content-Type",
                                                "application/x-www-form-urlencoded",
                                            )
                                            .body(Full::from(data)),
                                        Payload::Json(data) => res
                                            .header("Content-Type", "application/json")
                                            .body(Full::from(data)),
                                    }
                                }
                                Err(e) => Response::builder()
                                    .status(StatusCode::INTERNAL_SERVER_ERROR)
                                    .body(Full::from(e.to_string())),
                            }
                        } else {
                            Response::builder()
                                .status(StatusCode::BAD_REQUEST)
                                .body(Full::from(
                                    format!("Could not find a server function at the route {fn_name}. \
                                    \n\nIt's likely that you need to call ServerFn::register() on the \
                                    server function type, somewhere in your `main` function." )
                                ))
                        }
                        .expect("could not build Response");

                        _ = tx.send(res);
                    }
                })
        }
    });

    rx.await.unwrap()
}

pub type PinnedHtmlStream = Pin<Box<dyn Stream<Item = io::Result<Bytes>> + Send>>;

/// Returns an Axum [Handler](axum::handler::Handler) that listens for a `GET` request and tries
/// to route it using [leptos_router], serving an HTML stream of your application.
///
/// The provides a [MetaContext] and a [RouterIntegrationContext] to app’s context before
/// rendering it, and includes any meta tags injected using [leptos_meta].
///
/// The HTML stream is rendered using [render_to_stream], and includes everything described in
/// the documentation for that function.
///
/// This can then be set up at an appropriate route in your application:
/// ```
/// use axum::handler::Handler;
/// use axum::Router;
/// use std::{net::SocketAddr, env};
/// use leptos::*;
/// use leptos_config::get_configuration;
///
/// #[component]
/// fn MyApp(cx: Scope) -> impl IntoView {
///   view! { cx, <main>"Hello, world!"</main> }
/// }
///
/// # if false { // don't actually try to run a server in a doctest...
/// #[tokio::main]
/// async fn main() {
///     
///     let conf = get_configuration(Some("Cargo.toml")).await.unwrap();
///     let leptos_options = conf.leptos_options;
///     let addr = leptos_options.site_address.clone();
///     
///     // build our application with a route
///     let app = Router::new()
///     .fallback(leptos_axum::render_app_to_stream(leptos_options, |cx| view! { cx, <MyApp/> }));
///
///     // run our app with hyper
///     // `axum::Server` is a re-export of `hyper::Server`
///     axum::Server::bind(&addr)
///         .serve(app.into_make_service())
///         .await
///         .unwrap();
/// }
/// # }
/// ```
///
/// ## Provided Context Types
/// This function always provides context values including the following types:
/// - [RequestParts]
/// - [ResponseOptions]
/// - [MetaContext](leptos_meta::MetaContext)
/// - [RouterIntegrationContext](leptos_router::RouterIntegrationContext)
pub fn render_app_to_stream<IV>(
    options: LeptosOptions,
    app_fn: impl Fn(leptos::Scope) -> IV + Clone + Send + 'static,
) -> impl Fn(
    Request<Body>,
) -> Pin<Box<dyn Future<Output = Response<StreamBody<PinnedHtmlStream>>> + Send + 'static>>
       + Clone
       + Send
       + 'static
where
    IV: IntoView,
{
    render_app_to_stream_with_context(options, |_| {}, app_fn)
}

/// Returns an Axum [Handler](axum::handler::Handler) that listens for a `GET` request and tries
/// to route it using [leptos_router], serving an HTML stream of your application.
///
/// This version allows us to pass Axum State/Extension/Extractor or other infro from Axum or network
/// layers above Leptos itself. To use it, you'll need to write your own handler function that provides
/// the data to leptos in a closure. An example is below
/// ```ignore
/// async fn custom_handler(Path(id): Path<String>, Extension(options): Extension<Arc<LeptosOptions>>, req: Request<Body>) -> Response{
///     let handler = leptos_axum::render_app_to_stream_with_context((*options).clone(),
///     move |cx| {
///         provide_context(cx, id.clone());
///     },
///     |cx| view! { cx, <TodoApp/> }
/// );
///     handler(req).await.into_response()
/// }
/// ```
/// Otherwise, this function is identical to [render_app_to_stream].
///
/// ## Provided Context Types
/// This function always provides context values including the following types:
/// - [RequestParts]
/// - [ResponseOptions]
/// - [MetaContext](leptos_meta::MetaContext)
/// - [RouterIntegrationContext](leptos_router::RouterIntegrationContext)
pub fn render_app_to_stream_with_context<IV>(
    options: LeptosOptions,
    additional_context: impl Fn(leptos::Scope) + 'static + Clone + Send,
    app_fn: impl Fn(leptos::Scope) -> IV + Clone + Send + 'static,
) -> impl Fn(
    Request<Body>,
) -> Pin<Box<dyn Future<Output = Response<StreamBody<PinnedHtmlStream>>> + Send + 'static>>
       + Clone
       + Send
       + 'static
where
    IV: IntoView,
{
    move |req: Request<Body>| {
        Box::pin({
            let options = options.clone();
            let app_fn = app_fn.clone();
            let add_context = additional_context.clone();
            let default_res_options = ResponseOptions::default();
            let res_options2 = default_res_options.clone();
            let res_options3 = default_res_options.clone();

            async move {
                // Need to get the path and query string of the Request
                // For reasons that escape me, if the incoming URI protocol is https, it provides the absolute URI
                // if http, it returns a relative path. Adding .path() seems to make it explicitly return the relative uri
                let path = req.uri().path_and_query().unwrap().as_str();

                let full_path = format!("http://leptos.dev{path}");

                let pkg_path = &options.site_pkg_dir;
                let output_name = &options.output_name;

                // Because wasm-pack adds _bg to the end of the WASM filename, and we want to mantain compatibility with it's default options
                // we add _bg to the wasm files if cargo-leptos doesn't set the env var LEPTOS_OUTPUT_NAME
                // Otherwise we need to add _bg because wasm_pack always does. This is not the same as options.output_name, which is set regardless
                let mut wasm_output_name = output_name.clone();
                if std::env::var("LEPTOS_OUTPUT_NAME").is_err() {
                    wasm_output_name.push_str("_bg");
                }

                let site_ip = &options.site_address.ip().to_string();
                let reload_port = options.reload_port;

                let leptos_autoreload = match std::env::var("LEPTOS_WATCH").is_ok() {
                    true => format!(
                        r#"
                        <script crossorigin="">(function () {{
                            var ws = new WebSocket('ws://{site_ip}:{reload_port}/live_reload');
                            ws.onmessage = (ev) => {{
                                let msg = JSON.parse(ev.data);
                                if (msg.all) window.location.reload();
                                if (msg.css) {{
                                    const link = document.querySelector("link#leptos");
                                    if (link) {{
                                        let href = link.getAttribute('href').split('?')[0];
                                        let newHref = href + '?version=' + new Date().getMilliseconds();
                                        link.setAttribute('href', newHref);
                                    }} else {{
                                        console.warn("Could not find link#leptos");
                                    }}
                                }};
                            }};
                            ws.onclose = () => console.warn('Live-reload stopped. Manual reload necessary.');
                        }})()
                        </script>
                        "#
                    ),
                    false => "".to_string(),
                };

                let head = format!(
                    r#"<!DOCTYPE html>
                    <html lang="en">
                        <head>
                            <meta charset="utf-8"/>
                            <meta name="viewport" content="width=device-width, initial-scale=1"/>
                            <link rel="modulepreload" href="/{pkg_path}/{output_name}.js">
                            <link rel="preload" href="/{pkg_path}/{wasm_output_name}.wasm" as="fetch" type="application/wasm" crossorigin="">
                            <script type="module">import init, {{ hydrate }} from '/{pkg_path}/{output_name}.js'; init('/{pkg_path}/{wasm_output_name}.wasm').then(hydrate);</script>
                            {leptos_autoreload}
                            "#
                );
                let tail = "</body></html>";

                let (mut tx, rx) = futures::channel::mpsc::channel(8);

                spawn_blocking({
                    let app_fn = app_fn.clone();
                    let add_context = add_context.clone();
                    move || {
                        tokio::runtime::Runtime::new()
                            .expect("couldn't spawn runtime")
                            .block_on({
                                let app_fn = app_fn.clone();
                                let add_context = add_context.clone();
                                async move {
                                    tokio::task::LocalSet::new()
                                        .run_until(async {
                                            let app = {
                                                let full_path = full_path.clone();
                                                let req_parts = generate_request_parts(req).await;
                                                move |cx| {
                                                    let integration = ServerIntegration {
                                                        path: full_path.clone(),
                                                    };
                                                    provide_context(
                                                        cx,
                                                        RouterIntegrationContext::new(integration),
                                                    );
                                                    provide_context(cx, MetaContext::new());
                                                    provide_context(cx, req_parts);
                                                    provide_context(cx, default_res_options);
                                                    app_fn(cx).into_view(cx)
                                                }
                                            };

                                            let (bundle, runtime, scope) =
                                                render_to_stream_with_prefix_undisposed_with_context(
                                                    app,
                                                    |cx| {
                                                        let head = use_context::<MetaContext>(cx)
                                                            .map(|meta| meta.dehydrate())
                                                            .unwrap_or_default();
                                                        format!("{head}</head><body>").into()
                                                    },
                                                    add_context,
                                                );
                                            let mut shell = Box::pin(bundle);
                                            while let Some(fragment) = shell.next().await {
                                                _ = tx.send(fragment).await;
                                            }

                                            // Extract the value of ResponseOptions from here
                                            let cx = Scope { runtime, id: scope };
                                            let res_options =
                                                use_context::<ResponseOptions>(cx).unwrap();

                                            let new_res_parts = res_options.0.read().await.clone();

                                            let mut writable = res_options2.0.write().await;
                                            *writable = new_res_parts;

                                            runtime.dispose();

                                            tx.close_channel();
                                        })
                                        .await;
                                }
                            });
                    }
                });

                let mut stream = Box::pin(
                    futures::stream::once(async move { head.clone() })
                        .chain(rx)
                        .chain(futures::stream::once(async { tail.to_string() }))
                        .map(|html| Ok(Bytes::from(html))),
                );

                // Get the first, second, and third chunks in the stream, which renders the app shell, and thus allows Resources to run
                let first_chunk = stream.next().await;
                let second_chunk = stream.next().await;
                let third_chunk = stream.next().await;

                // Extract the resources now that they've been rendered
                let res_options = res_options3.0.read().await;

                let complete_stream = futures::stream::iter([
                    first_chunk.unwrap(),
                    second_chunk.unwrap(),
                    third_chunk.unwrap(),
                ])
                .chain(stream);

                let mut res = Response::new(StreamBody::new(
                    Box::pin(complete_stream) as PinnedHtmlStream
                ));

                if let Some(status) = res_options.status {
                    *res.status_mut() = status
                }
                let mut res_headers = res_options.headers.clone();
                res.headers_mut().extend(res_headers.drain());

                res
            }
        })
    }
}

/// Generates a list of all routes defined in Leptos's Router in your app. We can then use this to automatically
/// create routes in Axum's Router without having to use wildcard matching or fallbacks. Takes in your root app Element
/// as an argument so it can walk you app tree. This version is tailored to generate Axum compatible paths.
pub async fn generate_route_list<IV>(app_fn: impl FnOnce(Scope) -> IV + 'static) -> Vec<String>
where
    IV: IntoView + 'static,
{
    #[derive(Default, Clone, Debug)]
    pub struct Routes(pub Arc<RwLock<Vec<String>>>);

    let routes = Routes::default();
    let routes_inner = routes.clone();

    let local = LocalSet::new();
    // Run the local task set.

    local
        .run_until(async move {
            tokio::task::spawn_local(async move {
                let routes = leptos_router::generate_route_list_inner(app_fn);
                let mut writable = routes_inner.0.write().await;
                *writable = routes;
            })
            .await
            .unwrap();
        })
        .await;

    let routes = routes.0.read().await.to_owned();
    // Axum's Router defines Root routes as "/" not ""
    let routes: Vec<String> = routes
        .into_iter()
        .map(|s| if s.is_empty() { "/".to_string() } else { s })
        .collect();

    if routes.is_empty() {
        vec!["/".to_string()]
    } else {
        routes
    }
}

/// This trait allows one to pass a list of routes and a render function to Axum's router, letting us avoid
/// having to use wildcards or manually define all routes in multiple places.
pub trait LeptosRoutes {
    fn leptos_routes<IV>(
        self,
        options: LeptosOptions,
        paths: Vec<String>,
        app_fn: impl Fn(leptos::Scope) -> IV + Clone + Send + 'static,
    ) -> Self
    where
        IV: IntoView + 'static;
}
/// The default implementation of `LeptosRoutes` which takes in a list of paths, and dispatches GET requests
/// to those paths to Leptos's renderer.
impl LeptosRoutes for axum::Router {
    fn leptos_routes<IV>(
        self,
        options: LeptosOptions,
        paths: Vec<String>,
        app_fn: impl Fn(leptos::Scope) -> IV + Clone + Send + 'static,
    ) -> Self
    where
        IV: IntoView + 'static,
    {
        let mut router = self;
        for path in paths.iter() {
            router = router.route(
                path,
                get(render_app_to_stream(options.clone(), app_fn.clone())),
            );
        }
        router
    }
}
