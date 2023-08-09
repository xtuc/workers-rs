use proc_macro::TokenStream;
use quote::quote;
use syn::{parse_macro_input, punctuated::Punctuated, token::Comma, Ident, ItemFn};

pub fn expand_macro(attr: TokenStream, item: TokenStream) -> TokenStream {
    let attrs: Punctuated<Ident, Comma> =
        parse_macro_input!(attr with Punctuated::parse_terminated);

    enum HandlerType {
        Fetch,
        Scheduled,
        Start,
        #[cfg(feature = "queue")]
        Queue,
    }
    use HandlerType::*;

    let mut handler_type = None;
    let mut respond_with_errors = false;

    for attr in attrs {
        match attr.to_string().as_str() {
            "fetch" => handler_type = Some(Fetch),
            "scheduled" => handler_type = Some(Scheduled),
            "start" => handler_type = Some(Start),
            #[cfg(feature = "queue")]
            "queue" => handler_type = Some(Queue),
            "respond_with_errors" => {
                respond_with_errors = true;
            }
            _ => panic!("Invalid attribute: {}", attr),
        }
    }
    let handler_type = handler_type.expect(
        "must have either 'fetch', 'scheduled', 'queue' or 'start' attribute, e.g. #[event(fetch)]",
    );

    // create new var using syn item of the attributed fn
    let mut input_fn = parse_macro_input!(item as ItemFn);

    match handler_type {
        Fetch => {
            // TODO: validate the inputs / signature
            // save original fn name for re-use in the wrapper fn
            let input_fn_ident = Ident::new(
                &(input_fn.sig.ident.to_string() + "_fetch_glue"),
                input_fn.sig.ident.span(),
            );
            let wrapper_fn_ident = Ident::new("fetch", input_fn.sig.ident.span());
            // rename the original attributed fn
            input_fn.sig.ident = input_fn_ident.clone();

            let error_handling = match respond_with_errors {
                true => {
                    quote! {
                        Ok(::worker::worker_sys::web_sys::Response::from(
                            ::worker::Response::error(e.to_string(), 500).unwrap()
                        ).into())
                    }
                }
                false => {
                    quote! { panic!("{}", e) }
                }
            };

            // create a new "main" function that takes the worker_sys::Request, and calls the
            // original attributed function, passing in a converted worker::Request
            let wrapper_fn = quote! {
                pub fn #wrapper_fn_ident(
                    req: ::worker::worker_sys::web_sys::Request,
                    env: ::worker::Env,
                    ctx: ::worker::worker_sys::Context
                ) -> ::worker::js_sys::Promise {
                    let ctx = worker::Context::new(ctx);

                    let promise = ::worker::wasm_bindgen_futures::future_to_promise(async move {
                        // get the worker::Result<worker::Response> by calling the original fn
                        match #input_fn_ident(::worker::Request::from(req), env, ctx).await.map(::worker::worker_sys::web_sys::Response::from) {
                            Ok(res) => Ok(res.into()),
                            Err(e) => {
                                ::worker::console_error!("{}", &e);
                                #error_handling
                            }
                        }
                    });

                    // Wrap the user promise into our cancellable promise
                    // with an AbortController.
                    let abort_controller = Box::new(::worker::AbortController::default());
                    let promise = ::worker::cancellable_promise::make(abort_controller.signal(), promise);

                    // Save the AbortController.
                    *ABORT_CONTROLLER.lock().unwrap() = Some(abort_controller);

                    // Remove the AbortController once the Promise terminates.
                    let promise = {
                        let clean_abort_controller = ::worker::wasm_bindgen::closure::Closure::new(move || {
                            if let Ok(mut abort_controller) = ABORT_CONTROLLER.lock() {
                                *abort_controller = None;
                            }
                        });
                        // prevent the closure of being dropped before JS tries
                        // to call it.
                        let promise = promise.finally(&clean_abort_controller);
                        clean_abort_controller.forget();

                        promise
                    };

                    promise
                }
            };
            let wasm_bindgen_code =
                wasm_bindgen_macro_support::expand(TokenStream::new().into(), wrapper_fn)
                    .expect("wasm_bindgen macro failed to expand");

            let output = quote! {
                #input_fn

                use std::sync::Mutex;
                ::worker::lazy_static::lazy_static! {
                    // Keep the last request's AbortController, allowing the
                    // request to be aborted later if the code panics.
                    // Panics here cause the worker to hang,
                    // see https://github.com/rustwasm/wasm-bindgen/issues/2724.
                    //
                    // Only keeping the last request's AbortController may lead
                    // to some requests hanging, but it requires a lot of traffic
                    // that cause a panic.
                    //
                    // Note that while the worker is able to concurrently process
                    // request, we only keep the latest request's AbortController.
                    // This is to avoid cancelling a request from another request's
                    // context which leads to the error
                    // `Error: Cannot perform I/O on behalf of a different request...`.
                    static ref ABORT_CONTROLLER: Mutex<Option<Box<worker::AbortController>>> = Mutex::new(None);
                }

                #[no_mangle]
                pub extern "C" fn __workers_rs_cancel() {
                    if let Ok(controller) = ABORT_CONTROLLER.lock() {
                        if let Some(controller) = controller.as_ref() {
                            controller.abort();
                        }
                    }
                }

                mod _worker_fetch {
                    use ::worker::{wasm_bindgen, wasm_bindgen_futures};
                    use super::#input_fn_ident;
                    use super::ABORT_CONTROLLER;
                    #wasm_bindgen_code
                }
            };

            TokenStream::from(output)
        }
        Scheduled => {
            // save original fn name for re-use in the wrapper fn
            let input_fn_ident = Ident::new(
                &(input_fn.sig.ident.to_string() + "_scheduled_glue"),
                input_fn.sig.ident.span(),
            );
            let wrapper_fn_ident = Ident::new("scheduled", input_fn.sig.ident.span());
            // rename the original attributed fn
            input_fn.sig.ident = input_fn_ident.clone();

            let wrapper_fn = quote! {
                pub async fn #wrapper_fn_ident(event: ::worker::worker_sys::ScheduledEvent, env: ::worker::Env, ctx: ::worker::worker_sys::ScheduleContext) {
                    // call the original fn
                    #input_fn_ident(::worker::ScheduledEvent::from(event), env, ::worker::ScheduleContext::from(ctx)).await
                }
            };
            let wasm_bindgen_code =
                wasm_bindgen_macro_support::expand(TokenStream::new().into(), wrapper_fn)
                    .expect("wasm_bindgen macro failed to expand");

            let output = quote! {
                #input_fn

                mod _worker_scheduled {
                    use ::worker::{wasm_bindgen, wasm_bindgen_futures};
                    use super::#input_fn_ident;
                    #wasm_bindgen_code
                }
            };

            TokenStream::from(output)
        }
        #[cfg(feature = "queue")]
        Queue => {
            // save original fn name for re-use in the wrapper fn
            let input_fn_ident = Ident::new(
                &(input_fn.sig.ident.to_string() + "_queue_glue"),
                input_fn.sig.ident.span(),
            );
            let wrapper_fn_ident = Ident::new("queue", input_fn.sig.ident.span());
            // rename the original attributed fn
            input_fn.sig.ident = input_fn_ident.clone();

            let wrapper_fn = quote! {
                pub async fn #wrapper_fn_ident(event: ::worker::worker_sys::MessageBatch, env: ::worker::Env, ctx: ::worker::worker_sys::Context) {
                    // call the original fn
                    let ctx = worker::Context::new(ctx);
                    match #input_fn_ident(::worker::MessageBatch::new(event), env, ctx).await {
                        Ok(()) => {},
                        Err(e) => {
                            ::worker::console_log!("{}", &e);
                            panic!("{}", e);
                        }
                    }
                }
            };
            let wasm_bindgen_code =
                wasm_bindgen_macro_support::expand(TokenStream::new().into(), wrapper_fn)
                    .expect("wasm_bindgen macro failed to expand");

            let output = quote! {
                #input_fn

                mod _worker_queue {
                    use ::worker::{wasm_bindgen, wasm_bindgen_futures};
                    use super::#input_fn_ident;
                    #wasm_bindgen_code
                }
            };

            TokenStream::from(output)
        }
        Start => {
            // save original fn name for re-use in the wrapper fn
            let input_fn_ident = Ident::new(
                &(input_fn.sig.ident.to_string() + "_start_glue"),
                input_fn.sig.ident.span(),
            );
            let wrapper_fn_ident = Ident::new("start", input_fn.sig.ident.span());
            // rename the original attributed fn
            input_fn.sig.ident = input_fn_ident.clone();

            let wrapper_fn = quote! {
                pub fn #wrapper_fn_ident() {
                    // call the original fn
                    #input_fn_ident()
                }
            };
            let wasm_bindgen_code =
                wasm_bindgen_macro_support::expand(quote! { start }, wrapper_fn)
                    .expect("wasm_bindgen macro failed to expand");

            let output = quote! {
                #input_fn

                mod _worker_start {
                    use ::worker::{wasm_bindgen, wasm_bindgen_futures};
                    use super::#input_fn_ident;
                    #wasm_bindgen_code
                }
            };

            TokenStream::from(output)
        }
    }
}
