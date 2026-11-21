//! CEF requires Cocoa send-event state. Subclass GPUI's application so its
//! platform ivar and delegate remain intact while satisfying CEF's protocols.
use anyhow::{Context, Result, ensure};
use cocoa::base::id;
use objc::{
    class,
    declare::{ClassDecl, ProtocolDecl},
    msg_send,
    runtime::{BOOL, Class, NO, Object, Protocol, Sel, YES},
    sel, sel_impl,
};
use std::cell::Cell;

thread_local! { static SENDING_EVENT: Cell<BOOL> = const { Cell::new(NO) }; }
thread_local! {
    static QUIT_REQUESTED: Cell<bool> = const { Cell::new(false) };
    static CAN_TERMINATE: Cell<bool> = const { Cell::new(false) };
}

pub fn quit_requested() -> bool {
    QUIT_REQUESTED.with(Cell::get)
}
pub fn allow_termination() {
    CAN_TERMINATE.with(|state| state.set(true));
}

extern "C" fn terminate(this: &Object, _: Sel, sender: id) {
    if CAN_TERMINATE.with(Cell::get) {
        // GPUI's Cocoa terminate exits the process, so CEF must already be shut down.
        unsafe {
            let superclass = Class::get("GPUIApplication").unwrap_or(class!(NSApplication));
            let _: () = msg_send![super(this, superclass), terminate: sender];
        }
    } else {
        QUIT_REQUESTED.with(|state| state.set(true));
    }
}

extern "C" fn is_handling(_: &Object, _: Sel) -> BOOL {
    SENDING_EVENT.with(Cell::get)
}
extern "C" fn set_handling(_: &Object, _: Sel, value: BOOL) {
    SENDING_EVENT.with(|state| state.set(value));
}
extern "C" fn send_event(this: &Object, _: Sel, event: id) {
    let previous = SENDING_EVENT.with(|state| state.replace(YES));
    // SAFETY: this class inherits GPUIApplication; dispatch to its superclass.
    unsafe {
        let superclass = Class::get("GPUIApplication").unwrap_or(class!(NSApplication));
        let _: () = msg_send![super(this, superclass), sendEvent: event];
    }
    SENDING_EVENT.with(|state| state.set(previous));
}

pub fn prepare() -> Result<()> {
    let superclass =
        Class::get("GPUIApplication").context("GPUI application class is unavailable")?;
    let mut class = ClassDecl::new("VerifactApplication", superclass)
        .context("Unable to declare CEF-compatible GPUI application")?;
    // cef_application_mac.h defines this as an empty application-side protocol.
    // Chromium exports its parent protocols, but not this client declaration.
    if Protocol::get("CefAppProtocol").is_none() {
        let mut protocol =
            ProtocolDecl::new("CefAppProtocol").context("Unable to declare CefAppProtocol")?;
        protocol.add_protocol(
            Protocol::get("CrAppControlProtocol")
                .context("Missing Chromium CrAppControlProtocol")?,
        );
        protocol.register();
    }
    for name in ["CrAppProtocol", "CrAppControlProtocol", "CefAppProtocol"] {
        class.add_protocol(
            Protocol::get(name).with_context(|| format!("Missing CEF protocol {name}"))?,
        );
    }
    // SAFETY: signatures match Cocoa/CEF selectors and run on the main thread.
    unsafe {
        class.add_method(
            sel!(isHandlingSendEvent),
            is_handling as extern "C" fn(&Object, Sel) -> BOOL,
        );
        class.add_method(
            sel!(setHandlingSendEvent:),
            set_handling as extern "C" fn(&Object, Sel, BOOL),
        );
        class.add_method(
            sel!(sendEvent:),
            send_event as extern "C" fn(&Object, Sel, id),
        );
        class.add_method(
            sel!(terminate:),
            terminate as extern "C" fn(&Object, Sel, id),
        );
        let class = class.register();
        let app: id = msg_send![class, sharedApplication];
        let conforms: BOOL = msg_send![app, isKindOfClass: class];
        ensure!(
            conforms == YES,
            "NSApplication was created before the CEF/GPUI adapter"
        );
    }
    Ok(())
}
