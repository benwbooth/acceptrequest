use acceptrequest_common::BonusIncrement;
use leptos::prelude::*;

#[component]
pub fn App() -> impl IntoView {
    view! {
        <main>
            <h1>"acceptrequest"</h1>
            <BonusPicker />
        </main>
    }
}

#[component]
fn BonusPicker() -> impl IntoView {
    view! {
        <div class="bonus-picker">
            {BonusIncrement::PRESETS
                .iter()
                .map(|b| view! { <button>{format!("+{} min", b.minutes())}</button> })
                .collect_view()}
            <button>"Custom…"</button>
        </div>
    }
}
