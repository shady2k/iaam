//! Ядро учёта инвестиций.
//!
//! Чистые синхронные функции над загруженным срезом данных.
//! Ни ввода-вывода, ни `async`, ни `Mutex`, ни зависимостей на другие
//! крейты воркспейса. См. §3.1 спецификации.

pub mod contour;
pub mod dates;
pub mod event;
pub mod ids;
pub mod money;
pub mod numeric;
pub mod projection;
pub mod rules;
