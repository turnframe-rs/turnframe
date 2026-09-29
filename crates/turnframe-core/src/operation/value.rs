//! Values the model understands and code computes: dates and amounts of money.
//!
//! A model never computes «domani» or «10 euro»: it fills a [`DateExpr`] or a decimal
//! string, and code evaluates it against the turn's clock or the currency's exponent.

use chrono::{Datelike, Days, Months, NaiveDate, Weekday};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// A date as the user said it, for code to evaluate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum DateExpr {
    /// A calendar date; without a year, the argument's direction picks it.
    Absolute {
        /// The year, when the user said one.
        #[serde(default)]
        year: Option<i32>,
        /// Month, 1 to 12.
        month: u32,
        /// Day of the month.
        day: u32,
    },
    /// Some units from today: «domani» is one day, «tra due settimane» two weeks.
    Relative {
        /// The unit counted.
        unit: DateUnit,
        /// How many, negative for the past.
        amount: i32,
    },
    /// A day of the week.
    Weekday {
        /// Which day.
        day: DayOfWeek,
        /// Which occurrence of it.
        which: WeekdayOccurrence,
    },
    /// The last day of a period: «fine mese» is the end of this month.
    PeriodEnd {
        /// The period.
        period: DatePeriod,
        /// Which one.
        which: PeriodOccurrence,
    },
    /// The first day of a period.
    PeriodStart {
        /// The period.
        period: DatePeriod,
        /// Which one.
        which: PeriodOccurrence,
    },
}

/// A unit of [`DateExpr::Relative`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum DateUnit {
    /// Days.
    Day,
    /// Weeks of seven days.
    Week,
    /// Calendar months; the day is clamped to the month's length.
    Month,
    /// Calendar years.
    Year,
}

/// A day of the week.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum DayOfWeek {
    Monday,
    Tuesday,
    Wednesday,
    Thursday,
    Friday,
    Saturday,
    Sunday,
}

/// Which occurrence of a weekday.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum WeekdayOccurrence {
    /// The first one after today: «venerdì» said about a deadline.
    Coming,
    /// The last one before today.
    Previous,
    /// The one in the current week, Monday to Sunday.
    ThisWeek,
    /// The one in the following week.
    NextWeek,
    /// The one in the previous week.
    LastWeek,
}

/// A calendar period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum DatePeriod {
    Week,
    Month,
    Quarter,
    Year,
}

/// Which period.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
#[allow(missing_docs)]
pub enum PeriodOccurrence {
    This,
    Next,
    Last,
}

/// Which way a date without a year points.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DateDirection {
    /// The current year.
    #[default]
    Any,
    /// Today or later: a due date.
    Future,
    /// Today or earlier: a date something happened.
    Past,
}

/// Why a date expression names no date.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct DateError(pub String);

impl DateExpr {
    /// The date this expression names on `today`.
    ///
    /// # Errors
    ///
    /// [`DateError`] for a day that does not exist, such as the 31st of April.
    pub fn evaluate(
        &self,
        today: NaiveDate,
        direction: DateDirection,
    ) -> Result<NaiveDate, DateError> {
        let missing = || DateError(format!("{self:?} names no date"));
        match *self {
            Self::Absolute { year, month, day } => absolute(today, year, month, day, direction),
            Self::Relative { unit, amount } => relative(today, unit, amount).ok_or_else(missing),
            Self::Weekday { day, which } => Ok(weekday(today, day.into(), which)),
            Self::PeriodEnd { period, which } => period_bounds(today, period, which)
                .map(|(_, end)| end)
                .ok_or_else(missing),
            Self::PeriodStart { period, which } => period_bounds(today, period, which)
                .map(|(start, _)| start)
                .ok_or_else(missing),
        }
    }
}

fn absolute(
    today: NaiveDate,
    year: Option<i32>,
    month: u32,
    day: u32,
    direction: DateDirection,
) -> Result<NaiveDate, DateError> {
    let on = |year: i32| {
        NaiveDate::from_ymd_opt(year, month, day)
            .ok_or_else(|| DateError(format!("{year}-{month:02}-{day:02} does not exist")))
    };
    if let Some(year) = year {
        return on(year);
    }
    let this_year = on(today.year())?;
    Ok(match direction {
        DateDirection::Future if this_year < today => on(today.year() + 1)?,
        DateDirection::Past if this_year > today => on(today.year() - 1)?,
        _ => this_year,
    })
}

fn relative(today: NaiveDate, unit: DateUnit, amount: i32) -> Option<NaiveDate> {
    let magnitude = amount.unsigned_abs();
    let forward = amount >= 0;
    match unit {
        DateUnit::Day => shift_days(today, u64::from(magnitude), forward),
        DateUnit::Week => shift_days(today, u64::from(magnitude) * 7, forward),
        DateUnit::Month => shift_months(today, magnitude, forward),
        DateUnit::Year => shift_months(today, magnitude.checked_mul(12)?, forward),
    }
}

fn shift_days(date: NaiveDate, days: u64, forward: bool) -> Option<NaiveDate> {
    if forward {
        date.checked_add_days(Days::new(days))
    } else {
        date.checked_sub_days(Days::new(days))
    }
}

fn shift_months(date: NaiveDate, months: u32, forward: bool) -> Option<NaiveDate> {
    if forward {
        date.checked_add_months(Months::new(months))
    } else {
        date.checked_sub_months(Months::new(months))
    }
}

fn weekday(today: NaiveDate, day: Weekday, which: WeekdayOccurrence) -> NaiveDate {
    let monday = today - chrono::Duration::days(i64::from(today.weekday().num_days_from_monday()));
    let in_week = |week_start: NaiveDate| {
        week_start + chrono::Duration::days(i64::from(day.num_days_from_monday()))
    };
    match which {
        WeekdayOccurrence::ThisWeek => in_week(monday),
        WeekdayOccurrence::NextWeek => in_week(monday + chrono::Duration::days(7)),
        WeekdayOccurrence::LastWeek => in_week(monday - chrono::Duration::days(7)),
        WeekdayOccurrence::Coming => {
            let ahead = (7 + i64::from(day.num_days_from_monday())
                - i64::from(today.weekday().num_days_from_monday()))
                % 7;
            today + chrono::Duration::days(if ahead == 0 { 7 } else { ahead })
        }
        WeekdayOccurrence::Previous => {
            let behind = (7 + i64::from(today.weekday().num_days_from_monday())
                - i64::from(day.num_days_from_monday()))
                % 7;
            today - chrono::Duration::days(if behind == 0 { 7 } else { behind })
        }
    }
}

fn period_bounds(
    today: NaiveDate,
    period: DatePeriod,
    which: PeriodOccurrence,
) -> Option<(NaiveDate, NaiveDate)> {
    let step: i32 = match which {
        PeriodOccurrence::This => 0,
        PeriodOccurrence::Next => 1,
        PeriodOccurrence::Last => -1,
    };
    match period {
        DatePeriod::Week => {
            let monday = today
                - chrono::Duration::days(i64::from(today.weekday().num_days_from_monday()))
                + chrono::Duration::days(i64::from(step) * 7);
            Some((monday, monday + chrono::Duration::days(6)))
        }
        DatePeriod::Month => month_span(today, step, 1),
        DatePeriod::Quarter => {
            let first_month = (today.month0() / 3) * 3 + 1;
            let anchor = NaiveDate::from_ymd_opt(today.year(), first_month, 1)?;
            month_span(anchor, step * 3, 3)
        }
        DatePeriod::Year => {
            let year = today.year() + step;
            Some((
                NaiveDate::from_ymd_opt(year, 1, 1)?,
                NaiveDate::from_ymd_opt(year, 12, 31)?,
            ))
        }
    }
}

/// The span of `length` months starting `offset` months from `date`'s month.
fn month_span(date: NaiveDate, offset: i32, length: u32) -> Option<(NaiveDate, NaiveDate)> {
    let first = NaiveDate::from_ymd_opt(date.year(), date.month(), 1)?;
    let start = shift_months(first, offset.unsigned_abs(), offset >= 0)?;
    let end = start.checked_add_months(Months::new(length))? - chrono::Duration::days(1);
    Some((start, end))
}

impl From<DayOfWeek> for Weekday {
    fn from(day: DayOfWeek) -> Self {
        match day {
            DayOfWeek::Monday => Self::Mon,
            DayOfWeek::Tuesday => Self::Tue,
            DayOfWeek::Wednesday => Self::Wed,
            DayOfWeek::Thursday => Self::Thu,
            DayOfWeek::Friday => Self::Fri,
            DayOfWeek::Saturday => Self::Sat,
            DayOfWeek::Sunday => Self::Sun,
        }
    }
}

/// An amount of money in minor units: `1050` euro cents is 10.50 EUR.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct Money {
    /// The amount in the currency's minor unit.
    pub minor: i64,
    /// ISO 4217 code, upper case.
    pub currency: String,
}

/// Why a decimal string is not an amount of money.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct MoneyError(pub String);

impl Money {
    /// Parses `amount`, a decimal with a dot and no grouping, in `currency`.
    ///
    /// # Errors
    ///
    /// [`MoneyError`] for anything that is not such a decimal, for more decimals than
    /// the currency has, or for an amount out of range.
    pub fn parse(amount: &str, currency: &str) -> Result<Self, MoneyError> {
        let currency = currency.trim().to_ascii_uppercase();
        if currency.len() != 3 || !currency.chars().all(|c| c.is_ascii_uppercase()) {
            return Err(MoneyError(format!("`{currency}` is not an ISO 4217 code")));
        }
        let exponent = minor_exponent(&currency);
        let text = amount.trim();
        let (negative, digits) = match text.strip_prefix('-') {
            Some(rest) => (true, rest),
            None => (false, text),
        };
        let (whole, fraction) = digits.split_once('.').unwrap_or((digits, ""));
        let decimal = |part: &str| part.chars().all(|c| c.is_ascii_digit());
        if whole.is_empty() || !decimal(whole) || !decimal(fraction) {
            return Err(MoneyError(format!(
                "`{amount}` is not a decimal written with digits and one dot"
            )));
        }
        if fraction.len() > exponent as usize {
            return Err(MoneyError(format!(
                "`{amount}` has more decimals than {currency} allows ({exponent})"
            )));
        }
        let scale = 10_i64.pow(exponent);
        let out_of_range = || MoneyError(format!("`{amount}` is out of range"));
        let whole: i64 = whole.parse().map_err(|_| out_of_range())?;
        let padded = format!("{fraction:0<width$}", width = exponent as usize);
        let fraction: i64 = if padded.is_empty() {
            0
        } else {
            padded.parse().map_err(|_| out_of_range())?
        };
        let minor = whole
            .checked_mul(scale)
            .and_then(|minor| minor.checked_add(fraction))
            .ok_or_else(out_of_range)?;
        Ok(Self {
            minor: if negative { -minor } else { minor },
            currency,
        })
    }
}

/// Decimal places of a currency's minor unit; two unless the currency has none.
fn minor_exponent(currency: &str) -> u32 {
    match currency {
        "JPY" | "KRW" | "VND" | "CLP" | "ISK" | "HUF" => 0,
        "BHD" | "KWD" | "OMR" | "JOD" | "TND" => 3,
        _ => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn day(y: i32, m: u32, d: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(y, m, d).unwrap()
    }

    /// Tuesday, 14 November 2023.
    fn today() -> NaiveDate {
        day(2023, 11, 14)
    }

    fn eval(expr: DateExpr) -> NaiveDate {
        expr.evaluate(today(), DateDirection::Future).unwrap()
    }

    #[test]
    fn tomorrow_and_the_end_of_the_month_are_computed_from_today() {
        let tomorrow = DateExpr::Relative {
            unit: DateUnit::Day,
            amount: 1,
        };
        assert_eq!(eval(tomorrow), day(2023, 11, 15));
        let end_of_month = DateExpr::PeriodEnd {
            period: DatePeriod::Month,
            which: PeriodOccurrence::This,
        };
        assert_eq!(eval(end_of_month), day(2023, 11, 30));
        let next_quarter_start = DateExpr::PeriodStart {
            period: DatePeriod::Quarter,
            which: PeriodOccurrence::Next,
        };
        assert_eq!(eval(next_quarter_start), day(2024, 1, 1));
    }

    #[test]
    fn a_date_without_a_year_follows_the_arguments_direction() {
        let march = DateExpr::Absolute {
            year: None,
            month: 3,
            day: 1,
        };
        assert_eq!(
            march.evaluate(today(), DateDirection::Future).unwrap(),
            day(2024, 3, 1)
        );
        assert_eq!(
            march.evaluate(today(), DateDirection::Past).unwrap(),
            day(2023, 3, 1)
        );
        let december = DateExpr::Absolute {
            year: None,
            month: 12,
            day: 1,
        };
        assert_eq!(
            december.evaluate(today(), DateDirection::Past).unwrap(),
            day(2022, 12, 1)
        );
        let impossible = DateExpr::Absolute {
            year: Some(2023),
            month: 4,
            day: 31,
        };
        assert!(impossible.evaluate(today(), DateDirection::Any).is_err());
    }

    #[test]
    fn weekdays_are_counted_from_the_week_today_is_in() {
        let friday = |which| DateExpr::Weekday {
            day: DayOfWeek::Friday,
            which,
        };
        assert_eq!(eval(friday(WeekdayOccurrence::Coming)), day(2023, 11, 17));
        assert_eq!(eval(friday(WeekdayOccurrence::NextWeek)), day(2023, 11, 24));
        assert_eq!(eval(friday(WeekdayOccurrence::Previous)), day(2023, 11, 10));
        let tuesday = DateExpr::Weekday {
            day: DayOfWeek::Tuesday,
            which: WeekdayOccurrence::Coming,
        };
        assert_eq!(eval(tuesday), day(2023, 11, 21), "coming never means today");
        let month_end = DateExpr::Relative {
            unit: DateUnit::Month,
            amount: 1,
        };
        assert_eq!(
            month_end
                .evaluate(day(2024, 1, 31), DateDirection::Any)
                .unwrap(),
            day(2024, 2, 29),
            "a month later is clamped to the month's length"
        );
    }

    #[test]
    fn amounts_are_parsed_into_minor_units_by_the_currencys_exponent() {
        assert_eq!(
            Money::parse("10.5", "eur").unwrap(),
            Money {
                minor: 1050,
                currency: "EUR".into()
            }
        );
        assert_eq!(Money::parse("1300", "EUR").unwrap().minor, 130_000);
        assert_eq!(Money::parse("-2.01", "EUR").unwrap().minor, -201);
        assert_eq!(Money::parse("1500", "JPY").unwrap().minor, 1500);
        assert!(Money::parse("10.505", "EUR").is_err());
        assert!(Money::parse("1,300", "EUR").is_err());
        assert!(Money::parse("10", "euro").is_err());
    }
}
