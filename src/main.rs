use anyhow::{anyhow, Result};
use regex::Regex;
use reqwest::blocking::Client;
use reqwest::header::HeaderValue;
use reqwest::Url;
use rustyline::Editor;
use serde::Deserialize;
use serde_json;
use std::convert::TryInto;
use std::fs;
use std::path::PathBuf;
#[macro_use]
extern crate lazy_static;
use std::process::{Command, Output};

trait MenuDisplay {
    fn display(&self);
}

#[derive(Deserialize)]
struct Repo {
    name: String,
    ssh_url: String,
}

impl MenuDisplay for Repo {
    fn display(&self) {
        print!("{}", self.name);
    }
}

#[derive(Deserialize, Debug)]
struct Org {
    login: String,
    repos_url: String,
    description: String,
}

impl MenuDisplay for Org {
    fn display(&self) {
        print!("{}", self.login);
    }
}

struct BasicAuth {
    username: String,
    credential: String,
}

struct GithubClient {
    client: Client,
    auth: BasicAuth,
}

struct LinkItem {
    url: String,
    rel: String,
}

struct LinkHeader {
    items: Vec<LinkItem>,
}

impl LinkHeader {
    fn create(header: &HeaderValue) -> Result<LinkHeader> {
        let header_string = header.to_str()?;
        let item_parts = header_string.split(',');

        let mut items = Vec::new();
        for part in item_parts {
            items.push(LinkHeader::parse_item(part)?);
        }

        Ok(LinkHeader { items })
    }

    fn get_only_capture(regex: &Regex, value: &str) -> Option<String> {
        if let Some(captures) = regex.captures(value) {
            if let Some(value) = captures.get(1) {
                return Some(value.as_str().to_owned());
            }
        }
        None
    }

    fn parse_item(item: &str) -> Result<LinkItem> {
        lazy_static! {
            static ref LINK_RE: Regex = Regex::new(r"<(.+)>").unwrap();
            static ref REL_RE: Regex = Regex::new(r#"rel="?([^"]+)"?"#).unwrap();
        }

        let mut components = item.split(';');

        let url = match components.next() {
            Some(url_component) => LinkHeader::get_only_capture(&LINK_RE, url_component),
            None => None,
        };

        if let Some(url) = url {
            for component in components {
                if let Some(rel) = LinkHeader::get_only_capture(&REL_RE, component) {
                    return Ok(LinkItem { url, rel });
                }
            }
        }
        Err(anyhow!("Unable to parse {}", item))
    }

    fn find_rel(&self, rel: &str) -> Option<&LinkItem> {
        for item in &self.items {
            if item.rel.eq(rel) {
                return Some(item);
            }
        }
        None
    }
}

#[test]
fn test_create_link_header() -> Result<()> {
    let header =
        LinkHeader::create(&HeaderValue::from_str("<http://foo.bar/baz>; rel=\"next\"").unwrap())?;
    if let Some(rel) = header.find_rel("next") {
        println!("url={} rel={}", rel.url, rel.rel);
    }

    Ok(())
}

impl GithubClient {
    fn new(auth: BasicAuth) -> Result<GithubClient> {
        let http_client = Client::builder().user_agent("Repo-Exporter").build()?;
        let github_client = GithubClient {
            client: http_client,
            auth,
        };

        Ok(github_client)
    }

    fn fetch_repos(&self, repos_url: &str) -> Result<Vec<Repo>> {
        let url_with_params = Url::parse_with_params(repos_url, &[("per_page", "100")])?;
        let mut current_url = String::from(url_with_params.as_str());
        let mut all_results = Vec::new();
        let mut has_more_results = true;
        loop {
            println!("Fetching {}", &current_url);
            let response = self
                .client
                .get(&current_url)
                .basic_auth(&self.auth.username, Some(&self.auth.credential))
                .send()?;

            let link_header = response.headers().get("Link");
            match link_header {
                Some(ref link_header) => {
                    let link = LinkHeader::create(&link_header)?;
                    if let Some(rel) = link.find_rel("next") {
                        current_url.clone_from(&rel.url);
                    } else {
                        has_more_results = false;
                    }
                }
                None => {
                    return Ok(all_results);
                }
            }

            let mut response_repos: Vec<Repo> = serde_json::from_str(&(response.text()?))?;
            all_results.append(&mut response_repos);

            if !has_more_results {
                return Ok(all_results);
            }
        }
    }
    fn fetch_orgs(&self) -> Result<Vec<Org>> {
        let response_text = self
            .client
            .get("https://api.github.com/user/orgs")
            .basic_auth(&self.auth.username, Some(&self.auth.credential))
            .send()?
            .text()?;

        let response_orgs: Vec<Org> = serde_json::from_str(&response_text)?;

        Ok(response_orgs)
    }
}

fn show_menu<T: MenuDisplay>(items: &[T]) -> Option<usize> {
    for (x, item) in items.iter().enumerate() {
        print!("{}. ", (x + 1));
        item.display();
        println!();
    }

    let mut rl = Editor::<()>::new();
    loop {
        let line = rl.readline("Choice -> ");
        match line {
            Ok(line) => {
                let parsed = line.trim().parse::<i32>();
                match parsed {
                    Ok(number) => {
                        if number < 1 || number > items.len().try_into().unwrap() {
                            println!("Enter a number between 1 and {}", items.len());
                        } else {
                            return Some((number - 1).try_into().unwrap());
                        }
                    }
                    Err(_) => {
                        println!("Please enter a number");
                    }
                }
            }
            Err(_) => return None,
        }
    }
}

fn prompt_for_credentials() -> Result<BasicAuth> {
    println!("Generate a personal access token at https://github.com/settings/tokens");
    let mut rl = Editor::<()>::new();
    let username = rl.readline("Github username: ")?;
    let credential = rl.readline("Personal access token: ")?;

    Ok(BasicAuth {
        username,
        credential,
    })
}

fn prompt_for_directory() -> Result<PathBuf> {
    let mut rl = Editor::<()>::new();
    loop {
        let dir_str = rl.readline("Directory to export to: ")?;
        let path = PathBuf::from(&dir_str);
        if path.is_dir() && path.read_dir()?.count() > 0 {
            println!("Directory already exists and is not empty.");
        } else {
            fs::create_dir_all(path)?;
            return Ok(PathBuf::from(&dir_str));
        }
    }
}

fn run_clone(ssh_url: &str, dir: &PathBuf) -> Result<Output> {
    let output = Command::new("git")
        .args(&["clone", ssh_url])
        .current_dir(dir)
        .spawn()?
        .wait_with_output()?;
    Ok(output)
}

fn main() -> Result<()> {
    println!("Archiver v0.0");
    let auth = prompt_for_credentials()?;
    let client = GithubClient::new(auth)?;

    let orgs = client.fetch_orgs()?;
    println!("Choose a Github organization");
    let org_index = show_menu(&orgs[..]);
    if let Some(org_index) = org_index {
        let dir = prompt_for_directory()?;

        let repos = client.fetch_repos(&orgs[org_index].repos_url)?;
        for repo in repos {
            run_clone(&repo.ssh_url, &dir)?;
        }
    }
    Ok(())
}
