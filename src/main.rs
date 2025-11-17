use chrono::{Days, NaiveTime, TimeDelta, Timelike};
use clap::Parser;
use rusqlite::Row;
use rusqlite::config::DbConfig;

const HOME_DIR: &'static str = "tracc";
const DATABASE_FILE: &'static str = "db.sqlite";
const DT_FMT: &'static str = "%H:%M %d.%m.%y";

type LocalDT = chrono::DateTime<chrono::Local>;

fn get_database_connection() -> Result<rusqlite::Connection, String> {
    let mut path = match std::env::var("XDG_DATA_HOME") {
        Ok(v) => std::path::PathBuf::from(v),
        Err(v) => match v {
            std::env::VarError::NotPresent => std::env::home_dir()
                .map(|mut x| {
                    x.push(".local");
                    x.push("share");
                    x
                })
                .ok_or(format!("Could not determine home directory"))?,
            std::env::VarError::NotUnicode(_) => {
                return Err(format!(
                    "Could not get config home directory. Returned string was not unicode."
                ));
            }
        },
    };
    path.push(HOME_DIR);

    if !path.exists() {
        std::fs::create_dir_all(&path)
            .map_err(|err| format!("Could not create data directory: {err}"))?;
    } else {
        if path.is_file() {
            return Err(format!("Could not get data directory. Is a file."));
        }
    };
    path.push(DATABASE_FILE);

    // TODO: handle the error properly
    let conn = rusqlite::Connection::open(path)
        .map_err(|err| format!("Could not open database connection: {err}"))?;

    let ret = conn
        .set_db_config(DbConfig::SQLITE_DBCONFIG_ENABLE_FKEY, true)
        .map_err(|err| format!("Could not enable foreign key constraints: {err}"))?;

    Ok(conn)
}

struct App {
    conn: rusqlite::Connection,
    now: LocalDT,
}

impl App {
    pub fn try_init() -> Result<Self, String> {
        let conn = get_database_connection()?;
        let now = chrono::Local::now();

        let _ = conn
            .execute(
                "create table if not exists entries (
id INTEGER, 
datetime INTEGER,
kind INTEGER,
PRIMARY KEY(id)
);",
                (),
            )
            .map_err(|err| format!("Could not create entries table: {err}"))?;

        Ok(App { conn, now })
    }

    pub fn get_last_entry(&self) -> Result<Option<Entry>, String> {
        let mut statement = self
            .conn
            .prepare("SELECT * FROM entries where datetime = (SELECT MAX(datetime) from entries);")
            .map_err(|err| format!("Could not prepare statement: {err}"))?;
        let mut rows = statement
            .query(())
            .map_err(|err| format!("Could not get last entry: {err}"))?;

        let last_entry = match rows.next() {
            Ok(Some(row)) => Entry::from_db_row(row)?,
            Ok(None) => {
                return Ok(None);
            }
            Err(_) => todo!(),
        };

        if matches!(rows.next(), Ok(Some(_))) {
            return Err(format!(
                "More than one entry with the maximum timestamp. wow! This is an error, but very unlikely and we don't want to deal with that yet."
            ));
        }

        Ok(Some(last_entry))
    }

    pub fn add_begin(&mut self) -> Result<(), String> {
        let last_entry = self.get_last_entry()?;

        match last_entry {
            Some(Entry::Begin(date_time)) => {
                return Err(format!(
                    "Cannot start period. Current period started at {} is still running.",
                    date_time.format(DT_FMT)
                ));
            }
            Some(Entry::End(date_time)) => {
                println!(
                    "Starting new period. Last one ended at {}",
                    date_time.format(DT_FMT)
                )
            }
            None => {
                println!("Starting new period.",)
            }
        }

        self.conn
            .execute(
                "INSERT INTO entries (datetime, kind) VALUES (?1, 0)",
                (self.now.timestamp(),),
            )
            .map_err(|err| format!("Could not insert new begin entry: {err}"))?;

        Ok(())
    }

    pub fn add_end(&mut self) -> Result<(), String> {
        let last_entry = self.get_last_entry()?;

        match last_entry {
            Some(Entry::Begin(date_time)) => {
                println!("Ending period started at {}", date_time.format(DT_FMT))
            }
            Some(Entry::End(date_time)) => {
                return Err(format!(
                    "Cannot end period. Last period has already been ended at {}.",
                    date_time.format(DT_FMT)
                ));
            }
            None => return Err(format!("Cannot insert end entry as first entry")),
        }

        self.conn
            .execute(
                "INSERT INTO entries (datetime, kind) VALUES (?1, 1)",
                (self.now.timestamp(),),
            )
            .map_err(|err| format!("Could not insert new end entry: {err}"))?;

        Ok(())
    }

    fn show(&self) -> Result<(), String> {
        let mut query = self
            .conn
            .prepare("SELECT * FROM entries order by datetime;")
            .map_err(|err| format!("Could prepare entries query: {err}"))?;
        let mut entries = query
            .query(())
            .map_err(|err| format!("Could not query entries: {err}"))?;

        while let Ok(Some(row)) = entries.next() {
            let entry = Entry::from_db_row(row)?;

            match entry {
                Entry::Begin(dt) => {
                    println!("BEGIN: {}", dt.format(DT_FMT));
                }
                Entry::End(dt) => {
                    println!("END:   {}", dt.format(DT_FMT));
                }
            }
        }

        Ok(())
    }

    fn today(&self) -> Result<(), String> {
        let today_start = self
            .now
            .with_time(NaiveTime::from_hms_opt(0, 0, 0).expect("is valid"))
            .unwrap();

        let today_end = today_start
            .checked_add_days(Days::new(1))
            .expect("is inside of range");

        let mut query = self
            .conn
            .prepare(
                "SELECT * FROM entries where datetime >= ?1 and datetime < ?2 order by datetime;",
            )
            .map_err(|err| format!("Could prepare entries query: {err}"))?;

        let mut entries = query
            .query((today_start.timestamp(), today_end.timestamp()))
            .map_err(|err| format!("Could not query entries: {err}"))?;

        let mut time = TimeDelta::zero();

        let mut current_slot_begin = None;
        while let Ok(Some(row)) = entries.next() {
            let entry = Entry::from_db_row(row)?;

            match entry {
                Entry::Begin(dt) => {
                    current_slot_begin = Some(dt)
                }
                Entry::End(dt) => {
                    if let Some(begin) = current_slot_begin {
                        time += dt - begin;
                        current_slot_begin = None;
                    } else {
                        return Err(format!(
                            "Corrupted database. End at {} without previous period begin.",
                            dt.format(DT_FMT)
                        ));
                    }
                }
            }
        }
        if let Some(begin) = current_slot_begin {
            time += self.now - begin;
        }
        if time.num_days() != 0 {
            return Err(format!("Error with timedelta calculation. Number of days cannot be greater than 0. this must be a database corruption issue."));
        }
        println!("Total time spent today: {:2}:{:02}", time.num_hours(), time.num_minutes() - time.num_hours() * 60);

        Ok(())
    }
}

#[derive(clap::Parser, Debug)]
enum Command {
    Begin,
    End,
    Show,
    Today,
}

enum Entry {
    Begin(LocalDT),
    End(LocalDT),
}

fn import_datetime(x: i64) -> LocalDT {
    chrono::DateTime::from_timestamp(x, 0)
        .unwrap()
        .with_timezone(&chrono::Local)
}

impl Entry {
    pub fn from_db_row(row: &Row) -> Result<Entry, String> {
        let timestamp: LocalDT = row
            .get("datetime")
            .map(import_datetime)
            .map_err(|err| format!("Could not get datetime from row: {err}"))?;

        let kind: i64 = row
            .get("kind")
            .map_err(|err| format!("Could not get datetime from row: {err}"))?;
        Ok(match kind {
            0 => Entry::Begin(timestamp),
            1 => Entry::End(timestamp),
            _ => {
                return match row.get::<_, i64>("id") {
                    Ok(id) => Err(format!(
                        "Corrupted database contents: Found entry kind {kind} at id {id}. Expected 0 (Begin) or 1 (End)."
                    )),
                    Err(other_err) => Err(format!(
                        "Corrupted database contents: Found entry kind {kind}. Expected 0 (Begin) or 1 (End). Another error occurred when trying to get the corresponding entry id: {other_err}."
                    )),
                };
            }
        })
    }
}

fn main() {
    let mut app = App::try_init().unwrap_or_else(|err| {
        eprintln!("Could not initialize application: {err}");
        std::process::exit(1);
    });

    let args = Command::parse();
    match args {
        Command::Begin => app.add_begin(),
        Command::End => app.add_end(),
        Command::Show => app.show(),
        Command::Today => app.today(),
    }
    .unwrap_or_else(|err| {
        eprintln!("{err}");
        std::process::exit(1);
    });
}
