use crossterm::style::{Color, SetForegroundColor, ResetColor};
use std::fmt;

pub enum ActionType {
    Remove,
    Add,
    Overwrite,
    Append,
    Move,
    Skip,
}

impl fmt::Display for ActionType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ActionType::Remove => write!(f, "REMOVE"),
            ActionType::Add => write!(f, "ADD"),
            ActionType::Overwrite => write!(f, "OVERWRITE"),
            ActionType::Append => write!(f, "APPEND"),
            ActionType::Move => write!(f, "MOVE"),
            ActionType::Skip => write!(f, "SKIP"),
        }
    }
}

pub fn print_dry_run(action: ActionType, path: &str, details: Option<&str>) {
    let color = match action {
        ActionType::Remove => Color::Red,
        ActionType::Add => Color::Green,
        ActionType::Overwrite => Color::Yellow,
        ActionType::Append => Color::Blue,
        ActionType::Move => Color::Cyan,
        ActionType::Skip => Color::DarkGrey,
    };

    print!("{}", SetForegroundColor(color));
    print!("{:<10} ", action);
    print!("{}", ResetColor);
    
    print!("{}", path);
    
    if let Some(detail) = details {
        print!(" -> {}", detail);
    }
    
    println!();
}
