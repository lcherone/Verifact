//! Control experiment: GPUI rendering without CEF initialized.
use gpui::{prelude::*, *};
struct Control;
impl Render for Control {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .size_full()
            .bg(rgb(0x131820))
            .text_color(rgb(0xffffff))
            .text_size(px(24.))
            .p_6()
            .child("GPUI rendering control")
            .child(div().size(px(100.)).bg(rgb(0x32bc91)))
    }
}
fn main() {
    Application::new().run(|cx| {
        if let Err(error) = cx.open_window(WindowOptions::default(), |_, cx| cx.new(|_| Control)) {
            eprintln!("GPUI control failed: {error}");
            cx.quit();
        }
        cx.activate(true);
    });
}
