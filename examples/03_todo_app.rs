//! 03 — Todo App
//!
//! The full stack: signals, dynamic lists (`{Vec<Element>}` children),
//! `<input>` with focus/typing/Enter handling, `hover:` styles and
//! `line-through` decorations.
//!
//! Run: `cargo run --bin 03_todo_app`
//! Web: `trunk serve web/03_todo_app.html`

use html_rs::{launch, ui, Element, Signal};

#[derive(Clone)]
struct Todo {
    text: String,
    done: bool,
}

fn main() {
    let todos = Signal::new(vec![Todo {
        text: "Ship the wgpu renderer".into(),
        done: true,
    }]);
    let draft = Signal::new(String::new());

    launch(move || {
        let items = todos.get();
        let remaining = items.iter().filter(|t| !t.done).count();

        // Dynamic list: build child elements from state, splice into `ui!`
        // with a single `{...}` child.
        let rows: Vec<Element> = items
            .iter()
            .enumerate()
            .map(|(i, todo)| {
                let text_class = if todo.done {
                    "line-through text-slate-500".to_string()
                } else {
                    "text-slate-100".to_string()
                };
                ui! {
                    <div class="flex items-center gap-3 px-4 py-3 bg-slate-800 rounded-xl w-full">
                        <p
                            class={text_class}
                            onclick={move |_| todos.update(|ts| { ts[i].done = !ts[i].done; })}
                        >{todo.text.clone()}</p>
                        <button
                            class="ml-auto px-3 py-1.5 bg-rose-600 text-white hover:bg-rose-500 rounded-lg text-sm cursor-pointer"
                            onclick={move || todos.update(|ts| { ts.remove(i); })}
                        >"Delete"</button>
                    </div>
                }
            })
            .collect();

        ui! {
            <div class="w-full h-full flex flex-col items-center bg-slate-900 p-10">
                <p class="text-3xl font-bold text-white mb-6">"Todo App"</p>

                <div class="flex justify-center gap-3 mb-8">
                    <input
                        class="w-80 px-4 py-3 bg-slate-800 border border-slate-700 rounded-xl text-slate-100"
                        placeholder="What needs to be done?"
                        value={draft.get()}
                        oninput={move |text: &str| draft.set(text.to_string())}
                        onenter={move |_| add_todo(&todos, &draft)}
                    />
                    <button
                        class="px-6 py-3 bg-emerald-600 hover:bg-emerald-500 text-white font-semibold rounded-xl cursor-pointer"
                        onclick={move || add_todo(&todos, &draft)}
                    >"Add"</button>
                </div>

                {if rows.is_empty() {
                    Element::new("p")
                        .class("text-slate-500")
                        .text("Nothing yet — add your first todo above.")
                } else {
                    Element::new("div")
                        .class("flex flex-col gap-2 w-full items-stretch")
                        .children(html_rs::child_of(rows))
                }}

                <p class="mt-8 text-sm text-slate-500">{format!("{remaining} remaining")}</p>
            </div>
        }
    })
    .title("Todos — html.rs")
    .size(760.0, 600.0)
    .run();
}

fn add_todo(todos: &Signal<Vec<Todo>>, draft: &Signal<String>) {
    let text = draft.get();
    if !text.trim().is_empty() {
        todos.update(|ts| ts.push(Todo { text: text.trim().to_string(), done: false }));
    }
    draft.set(String::new());
}
