use crate::core::Core;
use anyhow::Result;
use cursive::event::{Event, Key};
use cursive::traits::*;
use cursive::views::{
Button, Dialog, EditView, LinearLayout, Panel, ResizedView,
TextContent, TextView,
};
use cursive::Cursive;
use std::sync::{Arc, Mutex};
use tracing::*;