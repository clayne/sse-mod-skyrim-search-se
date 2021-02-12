use anyhow::Context;
use winapi::ctypes::{c_void, c_char};
use std::ffi::{CStr, CString};
use std::intrinsics::transmute;
use detour::static_detour;
use clap::{SubCommand, Arg, AppSettings};
use win_dbg_logger::output_debug_string;
use crate::db;
use rusqlite::{NO_PARAMS, Statement};
use std::option::NoneError;
use rusqlite::types::ValueRef;
use late_static::LateStatic;
use crate::log::Loggable;

static_detour! {
    static ProcessConsoleInput: fn(usize, i64, i64, i64);
}

const SKYRIM_SEARCH_COMMANDS: [&str; 4] = ["ss", "sss", "skyrimsearch", "skyrimsearchse"];

fn get_clap<'a, 'b>() -> clap::App<'a, 'b> {
    clap::App::new("skyrim-search-se")
        .version("0.1")
        .setting(AppSettings::DisableHelpSubcommand)
        .setting(AppSettings::InferSubcommands)
        .setting(AppSettings::VersionlessSubcommands)
        .setting(AppSettings::SubcommandRequiredElseHelp)
        .subcommand(SubCommand::with_name("query")
            .about("execute raw query")
            .setting(AppSettings::TrailingVarArg)
            .arg(Arg::with_name("sql")
                .help("SQLite SQL")
                .required(true)
                .multiple(true)
            )
            .arg(Arg::with_name("int-as-decimal")
                .long("int-as-decimal")
                .help("print integer in decimal format. \
                          otherwise, it's printed in hexademical format.")))
}

fn new_process_console_input(param1: usize, param2: i64, param3: i64, param4: i64) {
    let mut print_usage = false;
    let result: anyhow::Result<bool> = (|| {
        let input = match unsafe {
            CStr::from_ptr(*((param1 + 0x38) as *const *const c_char)).to_str()
        } {
            Ok(input) => input,
            Err(err) => {
                output_debug_string(err.to_string().as_str());
                return Ok(false);
            }
        };
        if input.len() == 0 {
            return Ok(false);
        }
        let input = match shlex::split(input) {
            Some(result) => result,
            None => {
                if let Some(command) = input.trim_start().split_ascii_whitespace().next() {
                    if SKYRIM_SEARCH_COMMANDS.contains(&command) {
                        print("skyrim-search-se: parse failed; falling back to skyrim engine");
                    }
                }
                return Ok(false);
            }
        };
        let command = input[0].to_ascii_lowercase();
        if !SKYRIM_SEARCH_COMMANDS.contains(&command.as_str()) {
            print_usage = command == "help";
            return Ok(false);
        }
        print(format!("this is test; input = {:?}", input));

        let matches = get_clap().get_matches_from_safe(input)?;
        print(format!("matches: {:?}", matches));
        if let Some(matches) = matches.subcommand_matches("query") {
            process_query_command(matches)?;
        }
        Ok(true)
    })();
    if let Err(ref err) = result {
        print(format!("{:#}", err));
    }
    if let Ok(false) = result {
        ProcessConsoleInput.call(param1, param2, param3, param4);
    }
    if print_usage {
        print("skyrim-search-se usage: ss --help");
    }
}

fn process_query_command(matches: &clap::ArgMatches) -> anyhow::Result<()> {
    let sql = matches.values_of("sql").unwrap().collect::<Vec<&str>>().join(" ");
    let print_int_as_decimal = matches.is_present("int-as-decimal");

    let db = db::DB.lock().unwrap();
    let mut stmt: Statement = db.prepare(sql.as_str()).context("prepare error")?;
    print(format!("stmt: {:?}", stmt));
    let mut rows = stmt.query(NO_PARAMS).context("query error")?;
    let column_count = match rows.column_count() {
        Some(count) => count,
        None => anyhow::bail!("no data"),
    };

    let mut ptable = prettytable::Table::new();
    let _: Result<(), NoneError> = try {
        let names = rows.column_names()?;
        ptable.set_format(*prettytable::format::consts::FORMAT_NO_BORDER_LINE_SEPARATOR);
        ptable.set_titles(
            names
                .into_iter()
                .map(prettytable::Cell::new)
                .collect()
        );
    };
    loop {
        let row = match rows.next().map_err(anyhow::Error::new) {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(err) => anyhow::bail!(err.context("rows.next() error")),
        };
        let mut cells = Vec::with_capacity(column_count);
        for i in 0..column_count {
            let column = row.get_raw(i);
            let repr = match column {
                ValueRef::Null => String::from("<null>"),
                ValueRef::Integer(v) => {
                    if print_int_as_decimal {
                        v.to_string()
                    } else {
                        format!("{:#x}", v)
                    }
                },
                ValueRef::Real(v) => v.to_string(),
                ValueRef::Text(v) => String::from_utf8_lossy(v).to_string(),
                ValueRef::Blob(v) => format!("<{}-byte blob>", v.len()),
            };
            cells.push(prettytable::Cell::new(repr.as_str()));
        }
        ptable.add_row(prettytable::Row::new(cells));
    }
    print(ptable.to_string());
    Ok(())
}

struct State {
    console_context: *const *const c_void,
    print_to_console: extern "C" fn(*const c_void, *const c_char, ...) -> (),
}
unsafe impl Sync for State {}
static S: LateStatic<State> = LateStatic::new();

pub(crate) fn print<T: Into<Vec<u8>>>(msg: T) {
    let msg = msg.into();
    let msg = String::from_utf8_lossy(msg.as_ref());
    let msgs = msg.split("\n");
    // The print_to_console's internal buffer size is 1024.
    // ensure each lines not to overflow
    let chunks = msgs.flat_map(|msg| msg.as_bytes().chunks(1024));
    let chunks: Vec<Result<CString, _>> = chunks.map(CString::new).collect();

    let result: anyhow::Result<()> = try {
        unsafe {
            let console_context = S.console_context;
            if *console_context != std::ptr::null() {
                for msg in chunks {
                    (S.print_to_console)(
                        *console_context,
                        "%s\0".as_ptr() as *const c_char,
                        msg?.as_c_str().as_ptr(),
                    );
                }
            }
        }
    };

    result.logging_ok();
}

pub(crate) unsafe fn init(image_base: usize) -> anyhow::Result<()> {
    LateStatic::assign(&S, State {
        console_context: transmute(image_base + 0x2f000f0),
        print_to_console: transmute(image_base + 0x85c290),
    });

    let target_addr = transmute(image_base + 0x2e75f0);
    ProcessConsoleInput.initialize(target_addr, new_process_console_input)
        .context("initialize")?;
    ProcessConsoleInput.enable().context("enable")?;

    Ok(())
}
