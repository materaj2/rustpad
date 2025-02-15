pub mod calibrate_web;

use std::{collections::HashMap, str::FromStr, time::Duration};

use anyhow::{anyhow, Context, Result};
use getset::Setters;
use reqwest::{
    blocking::{Client, ClientBuilder},
    header::{HeaderMap, HeaderName, HeaderValue},
    redirect::Policy,
    Url,
};

use crate::{
    calibrator::calibration_response::CalibrationResponse,
    config::{web_config::WebConfig, SubConfig},
    cypher_text::encode::Encode,
};

use super::{oracle_location::OracleLocation, Oracle};

#[derive(Setters)]
pub struct WebOracle {
    url: Url,
    config: WebConfig,
    web_client: Client,
    keyword_locations: Vec<KeywordLocation>,
    #[getset(set = "pub")]
    padding_error_response: Option<CalibrationResponse>,
}

impl Oracle for WebOracle {
    fn visit(oracle_location: &OracleLocation, oracle_config: &SubConfig) -> Result<Self> {
        let (url, web_client, keyword_locations, web_config) =
            build_web_oracle(oracle_location, oracle_config)?;

        let oracle = Self {
            url,
            config: web_config.clone(),
            web_client,
            keyword_locations,
            padding_error_response: None,
        };
        Ok(oracle)
    }

    fn ask_validation<'a>(&self, cypher_text: &'a impl Encode<'a>) -> Result<bool> {
        let (url, data, headers) = replace_keyword_occurrences(
            &self.url,
            &self.config,
            self.keyword_locations.iter(),
            &cypher_text.encode(),
        )
        .context("Replacing all occurrences of keyword failed")?;

        let request = if self.config.post_data().is_none() {
            self.web_client.get(url)
        } else {
            self.web_client.post(url)
        };
        let request = request.headers(headers);
        let request = match data {
            Some(data) => request.body(data),
            None => request,
        };

        let response = request.send().context("Sending request failed")?;
        let response = CalibrationResponse::from_response(response, *self.config.consider_body())?;

        let padding_error_response = self.padding_error_response.as_ref().expect("Web oracle not calibrated. We don't know how an (in)correct padding response looks like");

        Ok(response != *padding_error_response)
    }

    fn location(&self) -> OracleLocation {
        OracleLocation::Web(self.url.clone())
    }
    fn thread_delay(&self) -> u64 {
        *self.config.thread_delay()
    }
}

#[derive(Debug)]
enum KeywordLocation {
    Url,
    PostData,
    Headers(HashMap<usize, HeaderWithKeyword>),
}

#[derive(Debug)]
struct HeaderWithKeyword {
    keyword_in_name: bool,
    keyword_in_value: bool,
}

fn replace_keyword_occurrences<'a>(
    url: &Url,
    config: &WebConfig,
    keyword_locations: impl Iterator<Item = &'a KeywordLocation>,
    encoded_cypher_text: &str,
) -> Result<(Url, Option<String>, HeaderMap)> {
    let mut url = url.clone();
    let mut data = config.post_data().clone();
    let mut headers = None;

    for location in keyword_locations {
        match location {
            KeywordLocation::Url => {
                url = Url::parse(&url
                    .to_string()
                    .replace(config.keyword(), encoded_cypher_text)).expect("Target URL, which parsed correctly initially, doesn't parse any more after replacing the keyword");
            }
            KeywordLocation::PostData => {
                data = Some(
                    data.as_deref()
                        .expect(
                            "The keyword was found in the POST data, yet no POST data exists...",
                        )
                        .replace(config.keyword(), encoded_cypher_text),
                );
            }
            KeywordLocation::Headers(headers_with_keyword) => {
                headers = Some(
                    replace_keyword_in_headers(config, headers_with_keyword, encoded_cypher_text)
                        .context("Parsing headers failed")?,
                );
            }
        }
    }

    // maybe there are no headers to replace, in which case the `HeaderMap` hasn't been constructed. Do it now
    if headers.is_none() {
        headers = Some(
            replace_keyword_in_headers(config, &HashMap::new(), encoded_cypher_text)
                .context("Parsing headers failed")?,
        );
    }

    Ok((
        url,
         data,
         headers.expect("HeaderMap should have been constructed even if no replacement in the headers is required")))
}

fn replace_keyword_in_headers(
    config: &WebConfig,
    headers_with_keyword: &HashMap<usize, HeaderWithKeyword>,
    encoded_cypher_text: &str,
) -> Result<HeaderMap> {
    config
        .headers()
        .iter()
        .enumerate()
        .map(|(idx, (name, value))| {
            // check if this header contains the keyword
            let (header_name, header_value) = match headers_with_keyword.get(&idx) {
                // do `HeaderName/HeaderValue::from_str` right away so we can prevent some `clone`s
                Some(replace_location) => {
                    // replace if needed
                    let resulting_name = if replace_location.keyword_in_name {
                        HeaderName::from_str(&name.replace(config.keyword(), encoded_cypher_text))
                    } else {
                        HeaderName::from_str(name)
                    };

                    let resulting_value = if replace_location.keyword_in_value {
                        HeaderValue::from_str(&value.replace(config.keyword(), encoded_cypher_text))
                    } else {
                        HeaderValue::from_str(value)
                    };

                    (resulting_name, resulting_value)
                }
                None => (HeaderName::from_str(name), HeaderValue::from_str(value)),
            };

            Ok((
                header_name.context(format!("Header name invalid: {}", name))?,
                header_value.context(format!("Header value invalid: {}", value))?,
            ))
        })
        .collect::<Result<_>>()
}

/// Try to indicate where the keyword is as precisely as possible. This is to prevent unneeded `.replace`s on every value, every time a request is made
fn keyword_location(url: &Url, config: &WebConfig) -> Vec<KeywordLocation> {
    let mut keyword_locations = Vec::with_capacity(3);

    if url.to_string().contains(config.keyword()) {
        keyword_locations.push(KeywordLocation::Url);
    }

    if config
        .post_data()
        .as_deref()
        .unwrap_or_default()
        .contains(config.keyword())
    {
        keyword_locations.push(KeywordLocation::PostData);
    }

    let headers_with_keyword = config
        .headers()
        .iter()
        .enumerate()
        .filter_map(|(idx, (name, value))| {
            let keyword_in_name = name.contains(config.keyword());
            let keyword_in_value = value.contains(config.keyword());

            if keyword_in_name || keyword_in_value {
                Some((
                    idx,
                    HeaderWithKeyword {
                        keyword_in_name,
                        keyword_in_value,
                    },
                ))
            } else {
                None
            }
        })
        .collect::<HashMap<_, _>>();
    if !headers_with_keyword.is_empty() {
        keyword_locations.push(KeywordLocation::Headers(headers_with_keyword));
    }

    keyword_locations
}

fn build_web_oracle<'a>(
    oracle_location: &OracleLocation,
    oracle_config: &'a SubConfig,
) -> Result<(Url, Client, Vec<KeywordLocation>, &'a WebConfig)> {
    let url = match oracle_location {
        OracleLocation::Web(url) => url,
        OracleLocation::Script(_) => {
            panic!("Tried to visit the web oracle using a file path!");
        }
    };

    let oracle_config = match oracle_config {
        SubConfig::Web(config) => config,
        SubConfig::Script(_) => {
            panic!("Tried to visit the web oracle using script configs!");
        }
    };

    let keyword_locations = keyword_location(url, oracle_config);
    if keyword_locations.is_empty() {
        return Err(anyhow!(
            "Keyword not found in URL, headers, or POST data. Double check whether you indicated the cypher text's location. See `--keyword` for extra info"
        ));
    }

    let mut client_builder = ClientBuilder::new()
        .timeout(Duration::from_secs(*oracle_config.request_timeout()))
        .danger_accept_invalid_certs(*oracle_config.insecure())
        .user_agent(oracle_config.user_agent());
    if !oracle_config.redirect() {
        client_builder = client_builder.redirect(Policy::none());
    }
    if let Some(proxy) = oracle_config.proxy() {
        client_builder = client_builder.proxy(proxy.clone());
    }

    let web_client = client_builder.build().context("Web client setup failed")?;

    Ok((url.to_owned(), web_client, keyword_locations, oracle_config))
}
