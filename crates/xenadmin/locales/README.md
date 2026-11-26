# User-interface languages

Every user-visible string of the console is in the message catalogue
(`src/locale.rs`). English is compiled in and is the fallback for every other
language. `en.toml` here is the English catalogue exported from the code: one
section per area (`[main]`, `[ops]`, `[prov]`… plus `[errors]`,
`[message_names]` and `[message_bodies]` for the XAPI error and alert texts),
one `key = "text"` line per string, and comments naming the placeholders a
text takes. Do not edit `en.toml` by hand: the English texts live in the
`string_table!` blocks of `src/*_strings.rs` and `src/i18n.rs`, and
`cargo test -p xenadmin` fails when the file is out of date
(`XENADMIN_BLESS=1 cargo test -p xenadmin english_catalogue` rewrites it).

Only English ships today (port plan decision 5).

## Adding a language (for example pt-BR)

1. Copy `en.toml` to `pt-BR.toml` (the file name is the BCP 47 tag) and
   translate the values. Keep the keys and every `{placeholder}` as they are
   (`{0}`, `{1}` in `[errors]` and `[message_bodies]` are the XAPI
   parameters). Keys you leave out stay in English, so a translation can
   ship partially; delete the generated header comment.
2. List it in `LOCALES` in `src/locale.rs`:

   ```rust
   LocaleInfo {
       tag: "pt-BR",
       name: "Português (Brasil)",
       source: Source::Toml(include_str!("../locales/pt-BR.toml")),
   },
   ```

3. Run `cargo test -p xenadmin locale`. `shipped_catalogues_are_valid`
   reports unknown sections or keys and texts whose placeholders differ from
   the English ones.

The language then shows in Tools > Options > Language. "System default"
picks it when the operating system's preferred language is `pt-BR` (or
another `pt-*`). The choice is saved as `language` in `settings.toml` and
takes effect at once.

## Writing new UI code

Add each new string to the table of its area (`string_table!` in the
area's `*_strings.rs`), with a `///` comment naming its placeholders, and
read it through the table's accessor (`prov_strings().new_vm`). Do not
cache text built from catalogue strings in a `static`; use
`locale::Derived` when an API needs a `&'static str`, so it follows the
language.

The default egui fonts cover Latin, Greek and Cyrillic. A language with
other scripts also needs a font added to the egui context.
