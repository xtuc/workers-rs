use crate::js_sys::Promise;
use crate::wasm_bindgen::prelude::*;
use crate::AbortSignal;

pub fn make(signal: AbortSignal, p: Promise) -> Promise {
    Promise::new(&mut |resolve, reject| {
        let msg = "Request has been aborted";

        if signal.aborted() {
            reject
                .call1(&JsValue::undefined(), &msg.into())
                .unwrap_throw();
        }

        {
            let reject = reject.clone();
            let on_abort = Closure::<dyn FnMut(JsValue)>::new(move |_| {
                reject
                    .call1(&JsValue::undefined(), &msg.into())
                    .unwrap_throw();
            });
            signal.set_onabort(Some(&on_abort.as_ref().unchecked_ref()));
            // prevent the closure of being dropped before JS tries
            // to call it.
            on_abort.forget();
        }

        // Listen for the initial promise completion
        {
            let resolve2 = Closure::new(move |val| {
                resolve.call1(&JsValue::undefined(), &val).unwrap_throw();
            });
            let reject2 = Closure::new(move |val| {
                reject.call1(&JsValue::undefined(), &val).unwrap_throw();
            });
            p.then2(&resolve2, &reject2);
        }
        ()
    })
}
