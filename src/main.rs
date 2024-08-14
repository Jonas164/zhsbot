use chrono::{NaiveDate, NaiveTime};
use clap::Parser;
use reqwest::{
    blocking::{get, Client, Response},
    Error,
};
use scraper::{selectable::Selectable, Html, Selector};
use std::{
    cmp::Ordering,
    collections::BTreeMap,
    fmt::{self},
    thread::sleep,
    time::Duration,
};
use timespan::NaiveTimeSpan;

enum Activity {
    BEACH,
    TENNIS,
    PICKLE,
}

#[derive(Parser, Debug)]
#[command(version, about, long_about = None)]
struct Args {
    //TODO let user decide which activity
    /// Date that should be watched for open courts
    #[arg(short, long)]
    date: String,

    //Exclusive After-Time. If you want a court at 14:00, type 13:30 here. Format is HH:MM
    #[arg(long)]
    after: Option<String>,

    //Exclusive Before-Time. If you want a court before 14:00, type 14:30 here  Format is HH:MM
    #[arg(long)]
    before: Option<String>,

    //Minimal time in minutes that should be available.
    #[arg(short, long)]
    length: Option<String>,
}

impl fmt::Display for Activity {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Activity::TENNIS => write!(f, "1"),
            Activity::BEACH => write!(f, "2"),
            Activity::PICKLE => write!(f, "3"),
        }
    }
}

struct Notifier {
    base_url: String,
    topic: String,
    client: Client,
}

struct UrlBuilder;

impl Notifier {
    fn notify(&self, message: String) -> Response {
        let url = format!("{}{}", self.base_url, self.topic);
        let res = self.client.post(url).body(message).send();

        match res {
            Ok(response) => return response,
            Err(e) => panic!("{}", e),
        }
    }
}

impl UrlBuilder {
    fn build_request_url(&self, activity: &Activity, date: &String, page_num: u8) -> String {
        return format!(
            "https://zhs-courtbuchung.de/reservations.php?action=showRevervations&type_id={type}&date={date}&page={page_num}",
            type = activity.to_string(),
            date = date,
            page_num = page_num.to_string()
        );
    }
}

struct Defaults {
    before: String,
    after: String,
    length: String,
}

fn main() {
    let time_fmt = "%H:%M";
    let date_fmt = "%d.%m.%Y";
    let notifier = Notifier {
        base_url: "https://ntfy.sh/".to_string(),
        topic: "zhsbot".to_string(),
        client: Client::new(),
    };

    let defaults = Defaults {
        before: "23:00".to_string(),
        after: "00:00".to_string(),
        length: "60".to_string(),
    };

    let args = Args::parse();

    let desired_date = NaiveDate::parse_from_str(&args.date, &date_fmt).unwrap();
    let desired_after_time =
        NaiveTime::parse_from_str(&args.after.unwrap_or(defaults.after), &time_fmt).unwrap();
    let desired_before_time =
        NaiveTime::parse_from_str(&args.before.unwrap_or(defaults.before), &time_fmt).unwrap();

    println!(
        "Searching for open courts on {} after {} and before {}, checking every 5s",
        desired_date, desired_after_time, desired_before_time
    );

    do_search(
        desired_date,
        desired_after_time,
        desired_before_time,
        notifier,
    );
}

fn do_search(
    desired_date: NaiveDate,
    desired_after_time: NaiveTime,
    desired_before_time: NaiveTime,
    notifier: Notifier,
) {
    loop {
        let available_times_per_court =
            query_and_parse(&Activity::TENNIS, &desired_date.to_string());

        match available_times_per_court {
            Some(available_times) => {
                let filtered =
                    filter_courts(available_times, desired_after_time, desired_before_time);
                if !filtered.is_empty() {
                    print_all_available_times(&filtered);
                    notifier.notify(build_result_string(&filtered));
                    return;
                }
                println!("Nothing found. Sleeping 5s")
            }
            None => panic!("Request or Parsing failed with error"),
        }
        sleep(Duration::from_secs(5));
    }
}

fn filter_courts(
    available_times: BTreeMap<u32, Vec<timespan::Span<chrono::NaiveTime>>>,
    desired_after_time: NaiveTime,
    desired_before_time: NaiveTime,
) -> BTreeMap<u32, Vec<timespan::Span<chrono::NaiveTime>>> {
    let mut filtered_courts: BTreeMap<u32, Vec<timespan::Span<NaiveTime>>> = BTreeMap::new();

    for (court, timeslots) in available_times {
        for slot in timeslots {
            if slot.start.cmp(&desired_after_time) == Ordering::Greater
                && slot.start.cmp(&desired_before_time) == Ordering::Less
            {
                //This slot matches
                filtered_courts.entry(court).or_default().push(slot.clone());
            }
        }
    }

    return filtered_courts;
}

fn print_all_available_times(available_times: &BTreeMap<u32, Vec<NaiveTimeSpan>>) {
    for (court, timeslots) in available_times {
        println!("Court {}", court);
        for slot in timeslots {
            println!("{}", slot);
        }
    }
}

fn build_result_string(filtered_available: &BTreeMap<u32, Vec<NaiveTimeSpan>>) -> String {
    if filtered_available.is_empty() {
        return "Sorry, nothing available".to_string();
    }

    let mut result = String::new();

    for (court, timeslots) in filtered_available {
        result.push_str("Court ");
        result.push_str(&court.to_string());
        result.push_str(":\n");
        for slot in timeslots {
            result.push_str(&slot.to_string());
            result.push_str("\n");
        }
    }
    return result;
}

fn query_and_parse(
    activity: &Activity,
    date: &String,
) -> Option<BTreeMap<u32, Vec<NaiveTimeSpan>>> {
    let url_builder = UrlBuilder {};

    //ZHS page is starting page_nums for TENNIS at 2 for some weird reason
    let mut page_num = match activity {
        Activity::BEACH => 1,
        Activity::TENNIS => 2,
        Activity::PICKLE => 1,
    };

    let mut result_map = BTreeMap::new();

    loop {
        let url = url_builder.build_request_url(activity, date, page_num);
        let response = perform_request(&url);

        match response {
            Ok(resp) => match resp.text() {
                Ok(resp_text) => {
                    let parsed_dom = Html::parse_document(&resp_text);

                    let court_tablecol_select =
                        Selector::parse("div.content > table > tbody > tr > td").unwrap();

                    let courts = parsed_dom.select(&court_tablecol_select);

                    //No more courts found
                    if courts.peekable().peek().is_none() {
                        return Some(result_map);
                    } else {
                        //For each table column representing one court
                        for court in parsed_dom.select(&court_tablecol_select) {
                            get_available_times_for_court(court, activity, &mut result_map);
                        }
                        page_num += 1;
                    }
                }
                Err(e) => {
                    println!("{:?}", e);
                    return None;
                }
            },
            Err(e) => {
                println!("{:?}", e.status());
                return None;
            }
        }
    }
}

fn get_available_times_for_court(
    court: scraper::ElementRef,
    activity: &Activity,
    result_map: &mut BTreeMap<u32, Vec<NaiveTimeSpan>>,
) {
    let per_court_available_time_select = Selector::parse("td.avaliable").unwrap();

    let court_num = get_court_num(court, activity);

    let mut available_timestamps = vec![];
    for available_timestamp in court.select(&per_court_available_time_select) {
        // println!("{:?}", tbody.text().collect::<Vec<_>>().concat());
        let times_string = available_timestamp.text().collect::<Vec<_>>().concat();
        available_timestamps.push(times_string);
    }

    if available_timestamps.len() == 0 {
        return;
    }

    let parsed_timespans: Vec<NaiveTimeSpan> = available_timestamps
        .iter()
        .map(|s| NaiveTimeSpan::parse_from_str(s.as_str(), "{start} - {end}", "%R", "%R").unwrap())
        .collect();

    // let compacted = compact_timespans(parsed_timespans); //TODO reenable once working

    result_map.insert(court_num, parsed_timespans);
}

fn compact_timespans(parsed_timespans: Vec<NaiveTimeSpan>) -> Vec<NaiveTimeSpan> {
    //TODO this is buggy and doesnt work at all
    let mut result: Vec<NaiveTimeSpan> = vec![];

    //Nothing to merge
    if parsed_timespans.len() == 1 {
        return parsed_timespans;
    }

    let mut it = parsed_timespans.iter().peekable();
    let mut curr: NaiveTimeSpan = it.next().unwrap().clone();
    let mut next: NaiveTimeSpan = it.next().unwrap().clone();

    loop {
        while curr.end == next.start && it.peek().is_some() {
            let mut merged = curr.union(&next).unwrap().clone();
            curr = merged;
            next = it.next().unwrap().clone();
        }
        result.push(curr.clone());
        if it.peek().is_none() {
            break;
        }
    }
    return result;

    // let mut i = 1;
    // while i < parsed_timespans.len() {
    //     let prev = parsed_timespans.get(i - 1).unwrap();
    //     let mut curr = parsed_timespans.get(i).unwrap();

    //     if prev.end == curr.start {
    //         curr = &prev.union(curr).unwrap();
    //         while (i < parsed_timespans.len())
    //         result.push(prev.union(curr).unwrap());
    //     } else {
    //         result.push(prev.to_owned());
    //     }
    // }
    // for i in 1..parsed_timespans.len() {
    //     let prev = parsed_timespans.get(i - 1).unwrap();
    //     let curr = parsed_timespans.get(i).unwrap();

    //     if prev.end == curr.start {
    //         result.push(prev.union(curr).unwrap());
    //     } else {
    //         result.push(prev.to_owned());
    //     }
    // }
}

//Getting the court name from the table header
fn get_court_num(tbodies: scraper::ElementRef, activity: &Activity) -> u32 {
    let name_prefix_len = match activity {
        Activity::BEACH => 5,
        Activity::TENNIS => 6,
        Activity::PICKLE => todo!(),
    };

    let court_num_select = Selector::parse("th").unwrap();

    let court_num = tbodies
        .select(&court_num_select)
        .next()
        .unwrap()
        .text()
        .collect::<Vec<_>>()
        .concat();
    return court_num[name_prefix_len..].parse::<u32>().unwrap();
}

fn perform_request(url: &str) -> Result<Response, Error> {
    return get(url);
}
