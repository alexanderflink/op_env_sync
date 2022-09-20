mod op_utils;
mod utils;
use argh::FromArgs;
use glob::glob;
use op_utils::{
    op_create_item, op_edit, op_field_in_section, op_field_to_env_var,
    op_field_to_env_var_reference, op_get_item, op_get_items, op_get_vaults, op_sign_in, op_whoami,
    OPField, OPItem, OPItemDetails, OPSection,
};
use std::env;
use std::fs;
use std::io::Write;
use std::path;
use std::process;
use utils::{
    ask_create_item, ask_proceed, ask_select_item, ask_select_items, env_vars_from_op_fields,
    get_argument_or_default, parse_env_file, read_env_file, write_to_file, EnvVariable,
};

#[derive(FromArgs)]
/// Sync environment variables using 1password.
struct Args {
    #[argh(subcommand)]
    subcommand: SubCommands,
}

#[derive(FromArgs, PartialEq, Debug)]
#[argh(subcommand)]
enum SubCommands {
    Up(SyncUpOptions),
    Down(SyncDownOptions),
}

#[derive(FromArgs, PartialEq, Debug)]
/// sync variables from .env file to 1password vault
#[argh(subcommand, name = "up")]
struct SyncUpOptions {
    #[argh(option, default = "String::from(\".env\")")]
    /// path to .env file
    env: String,
}

#[derive(FromArgs, PartialEq, Debug)]
/// sync variables from 1password vault to .env file
#[argh(subcommand, name = "down")]
struct SyncDownOptions {
    #[argh(option, default = "String::from(\".env.provision\")")]
    /// path to .env.provision file
    provision: String,
}

fn main() {
    let args: Args = argh::from_env();

    match args.subcommand {
        SubCommands::Up(options) => sync_up(options),
        SubCommands::Down(options) => sync_down(options),
    }

    println!("Done!");
}

fn sync_up(options: SyncUpOptions) {
    let op_signed_in = op_whoami();

    if !op_signed_in {
        println!("You are not logged in to 1password CLI. Proceeding to log in...");

        op_sign_in();
    }

    let env_file_path = options.env;

    let env_file_contents = match read_env_file(&env_file_path) {
        Ok(contents) => contents,
        Err(_) => {
            println!("Failed to read environment file at {}", &env_file_path);
            process::exit(1);
        }
    };

    let env_vars = parse_env_file(&env_file_contents);

    let vaults = op_get_vaults();

    let selected_vault =
        ask_select_item("Select vault: ", vaults).expect("Failed to select vault.");

    let mut items = op_get_items(&selected_vault);
    items.push(OPItem {
        title: String::from("(Create new)"),
        id: String::from("create-new"),
    });

    let item_details = match ask_select_item("Select item, or create new: ", items)
        .expect("Failed to select item.")
    {
        OPItem { id, .. } if id == String::from("create-new") => {
            let new_item_title = ask_create_item("Enter a name: ");

            op_create_item(&selected_vault.name, new_item_title.as_str())
        }
        item => op_get_item(item.id.as_str()),
    };

    let mut item_sections: Vec<OPSection> = item_details
        .sections
        .unwrap_or(Vec::new())
        .iter()
        .filter(|section| section.label.is_some())
        .map(|section| section.clone())
        .collect();

    item_sections.push(OPSection {
        label: Some(String::from("(Create new)")),
        id: String::from("create-new"),
    });

    item_sections.push(OPSection {
        label: Some(String::from("(None)")),
        id: String::from("none"),
    });

    let selected_section = match ask_select_item(
        "Select environment (e.g staging / production), or create new: ",
        item_sections,
    )
    .expect("Failed to select section.")
    {
        OPSection { id, .. } if id == String::from("create-new") => {
            let new_section_label = ask_create_item("Enter a name: ");

            Some(OPSection {
                label: Some(new_section_label.clone()),
                id: new_section_label.clone(),
            })
        }
        OPSection { id, .. } if id == String::from("none") => None,
        section => Some(section),
    };

    let item_vars: Vec<EnvVariable> = item_details
        .fields
        .iter()
        .filter(|field| op_field_in_section(field, &selected_section))
        .filter_map(op_field_to_env_var)
        .collect();

    let unsynced_env_vars: Vec<&EnvVariable> = env_vars
        .iter()
        .filter(|env| {
            item_vars.iter().all(|field_env| {
                // check if variable is not synced
                (field_env.key == env.key && field_env.value != env.value)
                    || field_env.key != env.key
            })
        })
        .collect();

    let env_vars_to_sync =
        match ask_select_items("Which variables do you want to sync?", unsynced_env_vars) {
            Ok(env_vars) => env_vars,
            Err(_) => {
                println!("All variables are up-to-date!");
                Vec::new()
            }
        };

    let confirmation_string = env_vars_to_sync.iter().fold(String::from(""), |acc, env| {
        format!("{}{} -> {}\n", acc, env.key, env.value)
    });

    if env_vars_to_sync.len() > 0
        && ask_proceed(
            format!(
                "Are you sure you want to sync these variables? \n{}",
                confirmation_string
            )
            .as_str(),
            false,
        )
    {
        let field_edit_command: Vec<String> = env_vars_to_sync
            .iter()
            .map(|env| match selected_section.clone() {
                Some(selected_section) => {
                    format!("{}.{}[text]={}", selected_section.id, env.key, env.value)
                }
                None => format!("{}[text]={}", env.key, env.value),
            })
            .collect();

        if op_edit(item_details.id.as_str(), field_edit_command).success() {
            println!("Synced variables successfully!");
        } else {
            println!("Failed to sync variables!");
        }
    }

    let provision_file_path = match selected_section.clone() {
        Some(OPSection {
            label: Some(label), ..
        }) => format!(".env.provision.{}", label),
        _ => String::from(".env.provision"),
    };

    // write to provision file
    if ask_proceed(
        format!("Do you want to write to {}?", &provision_file_path).as_str(),
        true,
    ) {
        let mut item_provision_vars: Vec<EnvVariable> = item_details
            .fields
            .iter()
            .filter(|field| op_field_in_section(field, &selected_section))
            .filter_map(op_field_to_env_var_reference)
            .collect();

        // check if provision file exists, if not, create it
        if path::Path::new(&provision_file_path).is_file() {
            let provision_file_contents =
                fs::read_to_string(&provision_file_path).expect("Failed to read provision file.");

            let provision_vars = parse_env_file(&provision_file_contents);

            item_provision_vars = item_provision_vars
                .into_iter()
                .filter(|item_var| {
                    provision_vars
                        .iter()
                        .all(|provision_var| provision_var.key != item_var.key)
                })
                .collect();
        } else {
            println!("Didn't find provision file, creating a new one!");
            fs::File::create(&provision_file_path).expect("Failed to create provision file.");
        }

        let string_to_write = item_provision_vars
            .iter()
            .fold(String::from(""), |acc, env| {
                format!("{}{}={}\n", acc, env.key, env.value)
            });

        fs::OpenOptions::new()
            .append(true)
            .open(&provision_file_path)
            .expect("Failed to open provision file")
            .write_all(string_to_write.as_bytes())
            .expect("Failed to write to provision file.");
    }
}

fn sync_down(options: SyncDownOptions) {
    // find all provision files
    let provision_files: Vec<glob::GlobResult> = glob("./.env.provision*")
        .expect("Failed to read glob pattern")
        .collect();

    println!("Found {} provision files", provision_files.len());

    // ask if user wants to write to provision files for each section

    // let user choose which provision file they want to use to sync to .env

    /*
        PROVISION FILE
    */

    // let provision_file_path = get_argument_or_default(1, ".env.provision");
    // let provision_file_contents = read_env_file(&provision_file_path).unwrap_or(String::new());
    // let provision_vars = parse_env_file(&provision_file_contents);
    // ask if user wants to sync to provision file
    // ask user which environments they want to write a provision file for
    // write fields in vault to corresponding provision file (section maps to .env.provision.[SECTION])

    // if ask_proceed(
    //     format!(
    //         "Do you want to sync variables from vault to provision file ({})?",
    //         provision_file_path
    //     )
    //     .as_str(),
    //     false,
    // ) {
    //     let use_env_for_section = ask_proceed(
    //         "Use environment variable $OP_ENV for environment (e.g staging / production)?",
    //         true,
    //     );

    //     let item_details = op_get_item(item_details.id.as_str());

    //     let item_fields = item_details.fields;

    //     let unsynced_fields: Vec<&OPField> = item_fields
    //         .iter()
    //         .filter(|field| match field.section.clone() {
    //             Some(field_section) => {
    //                 field_section.label == selected_section.label
    //                     && provision_vars.iter().all(|provision_var| {
    //                         provision_var.key != field.label.clone().unwrap_or(String::from(""))
    //                     })
    //             }
    //             None => false,
    //         })
    //         .collect();

    //     unsynced_fields.iter().for_each(|field| {
    //         let field_label = field.clone().label.clone();

    //         let selected_section_label = if use_env_for_section {
    //             Some(String::from("$OP_ENV"))
    //         } else {
    //             selected_section.label.clone()
    //         };

    //         match (field_label, selected_section_label) {
    //             (Some(field_label), Some(selected_section_label)) => {
    //                 write_to_file(
    //                     &provision_file_path,
    //                     format!(
    //                         "{}=op://{}/{}/{}/{}\n",
    //                         field_label,
    //                         selected_vault.name,
    //                         item_details.title,
    //                         selected_section_label,
    //                         field_label
    //                     )
    //                     .as_str(),
    //                 )
    //                 .expect("Failed to write to provision file.");
    //             }
    //             (Some(field_label), None) => {
    //                 write_to_file(
    //                     &provision_file_path,
    //                     format!(
    //                         "{}=op://{}/{}/{}/{}\n",
    //                         field_label,
    //                         selected_vault.name,
    //                         item_details.title,
    //                         selected_section.id,
    //                         field_label
    //                     )
    //                     .as_str(),
    //                 )
    //                 .expect("Failed to write to provision file.");
    //             }
    //             _ => (),
    //         }
    //     });
    // }
}
