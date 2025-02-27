use crate::measurements::format_bytes;
use crate::measurements::log_measurements;
use crate::measurements::Measurement;
use crate::progress::print_progress;
use crate::OutputFormat;
use crate::SpeedTestOptions;
use log;
use regex::Regex;
use reqwest::{blocking::Client, header::HeaderValue, StatusCode};
use serde::Serialize;
use std::{
    fmt::Display,
    time::{Duration, Instant},
};

const BASE_URL: &str = "http://speed.cloudflare.com";
const DOWNLOAD_URL: &str = "__down?bytes=";
const UPLOAD_URL: &str = "__up";

#[derive(Clone, Copy, Debug, Hash, Serialize, Eq, PartialEq)]
pub(crate) enum TestType {
    Download,
    Upload,
}

#[derive(Clone, Debug)]
pub(crate) enum PayloadSize {
    K100 = 100_000,
    M1 = 1_000_000,
    M10 = 10_000_000,
    M25 = 25_000_000,
    M100 = 100_000_000,
}

impl Display for PayloadSize {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", format_bytes(self.clone() as usize))
    }
}

impl PayloadSize {
    pub fn from(payload_string: String) -> Result<Self, String> {
        match payload_string.to_lowercase().as_str() {
            "100_000" | "100000" | "100k" | "100kb" => Ok(Self::K100),
            "1_000_000" | "1000000" | "1m" | "1mb" => Ok(Self::M1),
            "10_000_000" | "10000000" | "10m" | "10mb" => Ok(Self::M10),
            "25_000_000" | "25000000" | "25m" | "25mb" => Ok(Self::M25),
            "100_000_000" | "100000000" | "100m" | "100mb" => Ok(Self::M100),
            _ => Err("Value needs to be one of 100k, 1m, 10m, 25m or 100m".to_string()),
        }
    }

    fn sizes_from_max(max_payload_size: PayloadSize) -> Vec<usize> {
        log::debug!("getting payload iterations for max_payload_size {max_payload_size:?}");
        let payload_bytes: Vec<usize> =
            vec![100_000, 1_000_000, 10_000_000, 25_000_000, 100_000_000];
        match max_payload_size {
            PayloadSize::K100 => payload_bytes[0..1].to_vec(),
            PayloadSize::M1 => payload_bytes[0..2].to_vec(),
            PayloadSize::M10 => payload_bytes[0..3].to_vec(),
            PayloadSize::M25 => payload_bytes[0..4].to_vec(),
            PayloadSize::M100 => payload_bytes[0..5].to_vec(),
        }
    }
}

struct Metadata {
    city: String,
    country: String,
    ip: String,
    asn: String,
    colo: String,
}

impl Display for Metadata {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "City: {}\nCountry: {}\nIp: {}\nAsn: {}\nColo: {}",
            self.city, self.country, self.ip, self.asn, self.colo
        )
    }
}

pub(crate) fn speed_test(client: Client, options: SpeedTestOptions) {
    let metadata = fetch_metadata(&client);
    if options.outupt_format.is_none() {
        println!("{metadata}");
    }
    run_latency_test(&client, options.nr_latency_tests, options.outupt_format);
    let payload_sizes = PayloadSize::sizes_from_max(options.max_payload_size);
    let mut measurements = run_tests(
        &client,
        test_download,
        TestType::Download,
        payload_sizes.clone(),
        options.nr_tests,
        options.outupt_format,
    );
    measurements.extend(run_tests(
        &client,
        test_upload,
        TestType::Upload,
        payload_sizes.clone(),
        options.nr_tests,
        options.outupt_format,
    ));
    log_measurements(
        &measurements,
        payload_sizes,
        options.verbose,
        options.outupt_format,
    );
}

fn run_latency_test(
    client: &Client,
    nr_latency_tests: u32,
    outupt_format: Option<OutputFormat>,
) -> (Vec<f64>, f64) {
    let mut measurements: Vec<f64> = Vec::new();
    for i in 0..=nr_latency_tests {
        if outupt_format.is_none() {
            print_progress("latency test", i, nr_latency_tests);
        }
        let latency = test_latency(client);
        measurements.push(latency);
    }
    let avg_latency = measurements.iter().sum::<f64>() / measurements.len() as f64;
    if outupt_format.is_none() {
        println!(
            "\nAvg GET request latency {avg_latency:.2} ms (RTT excluding server processing time)\n"
        );
    }
    (measurements, avg_latency)
}

fn test_latency(client: &Client) -> f64 {
    let url = &format!("{}/{}{}", BASE_URL, DOWNLOAD_URL, 0);
    let req_builder = client.get(url);

    let start = Instant::now();
    let response = req_builder.send().expect("failed to get response");
    let _status_code = response.status();
    let duration = start.elapsed().as_secs_f64() * 1_000.0;

    let re = Regex::new(r"cfRequestDuration;dur=([\d.]+)").unwrap();
    let cf_req_duration: f64 = re
        .captures(
            response
                .headers()
                .get("Server-Timing")
                .expect("No Server-Timing in response header")
                .to_str()
                .unwrap(),
        )
        .unwrap()
        .get(1)
        .unwrap()
        .as_str()
        .parse()
        .unwrap();
    let mut req_latency = duration - cf_req_duration;
    if req_latency < 0.0 {
        // TODO investigate negative latency values
        req_latency = 0.0
    }
    req_latency
}
fn run_tests(
    client: &Client,
    test_fn: fn(&Client, usize, Option<OutputFormat>) -> f64,
    test_type: TestType,
    payload_sizes: Vec<usize>,
    nr_tests: u32,
    outupt_format: Option<OutputFormat>,
) -> Vec<Measurement> {
    let mut measurements: Vec<Measurement> = Vec::new();
    for payload_size in payload_sizes {
        log::debug!("running tests for payload_size {payload_size}");
        for i in 0..nr_tests {
            if outupt_format.is_none() {
                print_progress(
                    &format!("{:?} {:<5}", test_type, format_bytes(payload_size)),
                    i,
                    nr_tests,
                );
            }
            let mbit = test_fn(client, payload_size, outupt_format);
            measurements.push(Measurement {
                test_type,
                payload_size,
                mbit,
            });
        }
        if outupt_format.is_none() {
            print_progress(
                &format!("{:?} {:<5}", test_type, format_bytes(payload_size)),
                nr_tests,
                nr_tests,
            );
            println!()
        }
    }
    measurements
}

fn test_upload(
    client: &Client,
    payload_size_bytes: usize,
    outupt_format: Option<OutputFormat>,
) -> f64 {
    let url = &format!("{BASE_URL}/{UPLOAD_URL}");
    let payload: Vec<u8> = vec![1; payload_size_bytes];
    let req_builder = client.post(url).body(payload);
    let (status_code, mbits, duration) = {
        let start = Instant::now();
        let response = req_builder.send().expect("failed to get response");
        let status_code = response.status();
        let duration = start.elapsed();
        let mbits = (payload_size_bytes as f64 * 8.0 / 1_000_000.0) / duration.as_secs_f64();
        (status_code, mbits, duration)
    };
    if outupt_format.is_none() {
        print_current_speed(mbits, duration, status_code, payload_size_bytes);
    }
    mbits
}

fn test_download(
    client: &Client,
    payload_size_bytes: usize,
    outupt_format: Option<OutputFormat>,
) -> f64 {
    let url = &format!("{BASE_URL}/{DOWNLOAD_URL}{payload_size_bytes}");
    let req_builder = client.get(url);
    let (status_code, mbits, duration) = {
        let response = req_builder.send().expect("failed to get response");
        let status_code = response.status();
        let start = Instant::now();
        let _res_bytes = response.bytes();
        let duration = start.elapsed();
        let mbits = (payload_size_bytes as f64 * 8.0 / 1_000_000.0) / duration.as_secs_f64();
        (status_code, mbits, duration)
    };
    if outupt_format.is_none() {
        print_current_speed(mbits, duration, status_code, payload_size_bytes);
    }
    mbits
}

fn print_current_speed(
    mbits: f64,
    duration: Duration,
    status_code: StatusCode,
    payload_size_bytes: usize,
) {
    print!(
        "  {:>6.2} mbit/s | {:>5} in {:>4}ms -> status: {}  ",
        mbits,
        format_bytes(payload_size_bytes),
        duration.as_millis(),
        status_code
    );
}

fn fetch_metadata(client: &Client) -> Metadata {
    let url = &format!("{}/{}{}", BASE_URL, DOWNLOAD_URL, 0);
    let headers = client
        .get(url)
        .send()
        .expect("failed to get response")
        .headers()
        .to_owned();
    Metadata {
        city: extract_header_value(&headers, "cf-meta-city", "City N/A"),
        country: extract_header_value(&headers, "cf-meta-country", "Country N/A"),
        ip: extract_header_value(&headers, "cf-meta-ip", "IP N/A"),
        asn: extract_header_value(&headers, "cf-meta-asn", "ASN N/A"),
        colo: extract_header_value(&headers, "cf-meta-colo", "Colo N/A"),
    }
}

fn extract_header_value(
    headers: &reqwest::header::HeaderMap,
    header_name: &str,
    na_value: &str,
) -> String {
    headers
        .get(header_name)
        .unwrap_or(&HeaderValue::from_str(na_value).unwrap())
        .to_str()
        .unwrap()
        .to_owned()
}
