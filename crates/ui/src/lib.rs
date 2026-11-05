//! Dioxus components every Service-Arb frontend shares.

use dioxus::prelude::*;
use ev_lib::uikit::{Tooltip, TooltipContent};

/// What an action is expected to spend, shown on hover over it.
#[component]
pub fn TokenCost(tokens: i64, children: Element) -> Element {
	let mut open = use_signal(|| false);
	// not uikit's TooltipTrigger: it is a button, and the action under it is one too
	rsx! {
		Tooltip { open: open(),
			span {
				class: "inline-flex",
				onmouseenter: move |_| open.set(true),
				onmouseleave: move |_| open.set(false),
				onfocusin: move |_| open.set(true),
				onfocusout: move |_| open.set(false),
				{children}
			}
			TooltipContent { "≈ {tokens} tokens" }
		}
	}
}
