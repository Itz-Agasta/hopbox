//! vast.ai through `vastai … --raw`. hopbox only touches instances labelled `hopbox-<name>`.

use serde::Deserialize;

use super::Cli;

/// Default image: vast's PyTorch image with CUDA, tag picked for the host's driver.
pub const DEFAULT_IMAGE: &str = "vastai/pytorch:@vastai-automatic-tag";
/// Every instance hopbox creates carries this label prefix; nothing else is ever touched.
pub const LABEL_PREFIX: &str = "hopbox-";

#[derive(Debug, thiserror::Error)]
pub enum VastError {
    #[error("cannot run vastai: {0} (install it and run `vastai set api-key`)")]
    Spawn(#[from] std::io::Error),
    #[error("vastai {cmd} failed: {msg}")]
    Failed { cmd: String, msg: String },
    #[error("unexpected vastai {cmd} output: {msg}")]
    Parse { cmd: String, msg: String },
}

/// What to search for.
#[derive(Debug, Clone, PartialEq)]
pub struct Query {
    /// GPU model as vast names it; spaces become underscores (`RTX 4090` → `RTX_4090`).
    pub gpu: String,
    pub max_price: Option<f64>,
    /// Minimum disk in GB.
    pub disk: u32,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct Offer {
    pub id: u64,
    pub gpu_name: String,
    pub num_gpus: u32,
    pub dph_total: f64,
    pub reliability: f64,
    #[serde(default)]
    pub geolocation: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Deserialize)]
pub struct InstanceInfo {
    pub id: u64,
    #[serde(default)]
    pub actual_status: Option<String>,
    #[serde(default)]
    pub label: Option<String>,
    #[serde(default)]
    pub dph_total: f64,
    #[serde(default)]
    pub start_date: Option<f64>,
    #[serde(default)]
    pub gpu_name: Option<String>,
}

impl InstanceInfo {
    /// The hopbox name from a `hopbox-<name>` label.
    pub fn hopbox_name(&self) -> Option<&str> {
        self.label.as_deref()?.strip_prefix(LABEL_PREFIX)
    }
}

/// What `create` needs besides the offer.
#[derive(Debug, Clone, PartialEq)]
pub struct CreateSpec {
    pub name: String,
    pub image: String,
    pub disk: u32,
    /// Unix seconds; baked into the onstart watchdog.
    pub deadline: u64,
}

pub struct Vast<C: Cli> {
    pub cli: C,
}

fn args(v: &[&str]) -> Vec<String> {
    v.iter().map(|s| (*s).to_owned()).collect()
}

impl<C: Cli> Vast<C> {
    fn call(&self, cmd: &str, a: Vec<String>) -> Result<String, VastError> {
        let o = self.cli.run(&a)?;
        if o.code != 0 {
            let msg = if o.stderr.trim().is_empty() {
                o.stdout.trim()
            } else {
                o.stderr.trim()
            };
            return Err(VastError::Failed {
                cmd: cmd.to_owned(),
                msg: msg.to_owned(),
            });
        }
        Ok(o.stdout)
    }

    fn json<T: serde::de::DeserializeOwned>(cmd: &str, text: &str) -> Result<T, VastError> {
        serde_json::from_str(text).map_err(|e| VastError::Parse {
            cmd: cmd.to_owned(),
            msg: e.to_string(),
        })
    }

    /// Offers matching `q`, best `dlperf_usd` first. Only verified, rentable machines with
    /// direct ports and reliability above 0.98.
    pub fn search(&self, q: &Query) -> Result<Vec<Offer>, VastError> {
        let mut filters = format!(
            "verified=true rentable=true direct_port_count>=1 reliability>0.98 gpu_name={} disk_space>={}",
            q.gpu.replace(' ', "_"),
            q.disk
        );
        if let Some(max) = q.max_price {
            filters += &format!(" dph_total<={max}");
        }
        let out = self.call(
            "search offers",
            args(&["search", "offers", &filters, "-o", "dlperf_usd-", "--raw"]),
        )?;
        let offers: Vec<Offer> = Self::json("search offers", &out)?;
        // Re-check the price: never trust a filter alone with money.
        Ok(offers
            .into_iter()
            .filter(|o| q.max_price.is_none_or(|m| o.dph_total <= m))
            .collect())
    }

    /// Rent `offer` and return the new instance id.
    pub fn create(&self, offer: u64, spec: &CreateSpec) -> Result<u64, VastError> {
        #[derive(Deserialize)]
        struct Created {
            success: bool,
            new_contract: Option<u64>,
        }
        let out = self.call(
            "create instance",
            vec![
                "create".into(),
                "instance".into(),
                offer.to_string(),
                "--image".into(),
                spec.image.clone(),
                "--disk".into(),
                spec.disk.to_string(),
                "--ssh".into(),
                "--direct".into(),
                "--label".into(),
                format!("{LABEL_PREFIX}{}", spec.name),
                "--onstart-cmd".into(),
                watchdog(spec.deadline),
                "--raw".into(),
            ],
        )?;
        let c: Created = Self::json("create instance", &out)?;
        match c {
            Created {
                success: true,
                new_contract: Some(id),
            } => Ok(id),
            _ => Err(VastError::Parse {
                cmd: "create instance".into(),
                msg: "no new_contract in the reply".into(),
            }),
        }
    }

    pub fn show(&self, id: u64) -> Result<InstanceInfo, VastError> {
        let out = self.call(
            "show instance",
            vec![
                "show".into(),
                "instance".into(),
                id.to_string(),
                "--raw".into(),
            ],
        )?;
        Self::json("show instance", &out)
    }

    /// Instances whose label starts with `hopbox-`.
    pub fn list(&self) -> Result<Vec<InstanceInfo>, VastError> {
        let out = self.call("show instances", args(&["show", "instances", "--raw"]))?;
        let all: Vec<InstanceInfo> = Self::json("show instances", &out)?;
        Ok(all
            .into_iter()
            .filter(|i| i.hopbox_name().is_some())
            .collect())
    }

    /// `(user, host, port)` from `vastai ssh-url <id>` (`ssh://root@1.2.3.4:40022`).
    pub fn ssh_url(&self, id: u64) -> Result<(String, String, u16), VastError> {
        let out = self.call("ssh-url", vec!["ssh-url".into(), id.to_string()])?;
        parse_ssh_url(out.trim()).ok_or_else(|| VastError::Parse {
            cmd: "ssh-url".into(),
            msg: out.trim().to_owned(),
        })
    }

    pub fn destroy(&self, id: u64) -> Result<(), VastError> {
        self.call(
            "destroy instance",
            vec![
                "destroy".into(),
                "instance".into(),
                id.to_string(),
                "-y".into(),
            ],
        )
        .map(|_| ())
    }
}

pub fn parse_ssh_url(url: &str) -> Option<(String, String, u16)> {
    let rest = url.strip_prefix("ssh://")?;
    let (user, hostport) = rest.split_once('@')?;
    let (host, port) = hostport.rsplit_once(':')?;
    let port = port.trim_end_matches('/').parse().ok()?;
    (!user.is_empty() && !host.is_empty()).then(|| (user.to_owned(), host.to_owned(), port))
}

/// The onstart command that makes the instance destroy itself at `deadline`, with the
/// instance's own restricted key (it can only start, stop or destroy that one instance).
/// The deadline is absolute and written once, so a container restart can't extend it.
pub fn watchdog(deadline: u64) -> String {
    format!(
        concat!(
            "D=/var/tmp/hopbox-deadline; [ -f $D ] || echo {deadline} > $D; ",
            "(while [ \"$(date +%s)\" -lt \"$(cat $D)\" ]; do sleep 60; done; ",
            "while :; do curl -fsS -X DELETE -H \"Authorization: Bearer $CONTAINER_API_KEY\" ",
            "\"https://console.vast.ai/api/v0/instances/$CONTAINER_ID/\" && break; sleep 60; done) ",
            ">/dev/null 2>&1 &"
        ),
        deadline = deadline
    )
}

#[cfg(test)]
pub mod tests {
    use super::*;
    use crate::provider::CliOutput;
    use std::cell::RefCell;
    use std::collections::VecDeque;

    pub fn fixture(name: &str) -> String {
        std::fs::read_to_string(format!(
            "{}/tests/fixtures/vast/{name}",
            env!("CARGO_MANIFEST_DIR")
        ))
        .unwrap()
    }

    /// Replays canned replies in order and records every call.
    #[derive(Default)]
    pub struct FakeCli {
        pub replies: RefCell<VecDeque<CliOutput>>,
        pub calls: RefCell<Vec<Vec<String>>>,
    }

    impl FakeCli {
        pub fn with(replies: &[(i32, &str)]) -> FakeCli {
            FakeCli {
                replies: RefCell::new(
                    replies
                        .iter()
                        .map(|(code, out)| CliOutput {
                            code: *code,
                            stdout: (*out).to_owned(),
                            stderr: String::new(),
                        })
                        .collect(),
                ),
                calls: RefCell::default(),
            }
        }
    }

    impl Cli for FakeCli {
        fn run(&self, args: &[String]) -> std::io::Result<CliOutput> {
            self.calls.borrow_mut().push(args.to_vec());
            Ok(self
                .replies
                .borrow_mut()
                .pop_front()
                .unwrap_or_else(|| panic!("unexpected vastai call {args:?}")))
        }
    }

    impl Cli for &FakeCli {
        fn run(&self, args: &[String]) -> std::io::Result<CliOutput> {
            (*self).run(args)
        }
    }

    #[test]
    fn search_args_and_parse() {
        let v = Vast {
            cli: FakeCli::with(&[(0, &fixture("search-4090.json"))]),
        };
        let offers = v
            .search(&Query {
                gpu: "RTX 4090".into(),
                max_price: Some(0.6),
                disk: 30,
            })
            .unwrap();
        assert_eq!(offers.len(), 3);
        assert_eq!(offers[0].id, 48293175);
        assert_eq!(offers[0].gpu_name, "RTX 4090");
        assert!(offers[0].dph_total < 0.6);
        let call = &v.cli.calls.borrow()[0];
        assert_eq!(&call[..2], ["search", "offers"]);
        assert_eq!(
            call[2],
            "verified=true rentable=true direct_port_count>=1 reliability>0.98 gpu_name=RTX_4090 disk_space>=30 dph_total<=0.6"
        );
        assert_eq!(&call[3..], ["-o", "dlperf_usd-", "--raw"]);
    }

    #[test]
    fn up_rechecks_price() {
        // The CLI filter is not trusted: offers above the max are dropped locally.
        let v = Vast {
            cli: FakeCli::with(&[(0, &fixture("search-4090.json"))]),
        };
        let offers = v
            .search(&Query {
                gpu: "RTX_4090".into(),
                max_price: Some(0.01),
                disk: 30,
            })
            .unwrap();
        assert!(offers.is_empty());
    }

    #[test]
    fn create_args_include_watchdog_and_label() {
        let v = Vast {
            cli: FakeCli::with(&[(0, &fixture("create.json"))]),
        };
        let id = v
            .create(
                48293175,
                &CreateSpec {
                    name: "gpu1".into(),
                    image: DEFAULT_IMAGE.into(),
                    disk: 30,
                    deadline: 1_700_007_200,
                },
            )
            .unwrap();
        assert_eq!(id, 31337);
        let call = &v.cli.calls.borrow()[0];
        let pos = |flag: &str| call.iter().position(|a| a == flag).unwrap();
        assert_eq!(&call[..3], ["create", "instance", "48293175"]);
        assert_eq!(call[pos("--label") + 1], "hopbox-gpu1");
        assert_eq!(call[pos("--onstart-cmd") + 1], watchdog(1_700_007_200));
        assert!(call.contains(&"--ssh".to_owned()) && call.contains(&"--direct".to_owned()));
        assert_eq!(call.last().unwrap(), "--raw");
    }

    #[test]
    fn watchdog_golden() {
        assert_eq!(
            watchdog(1_700_007_200),
            concat!(
                "D=/var/tmp/hopbox-deadline; [ -f $D ] || echo 1700007200 > $D; ",
                "(while [ \"$(date +%s)\" -lt \"$(cat $D)\" ]; do sleep 60; done; ",
                "while :; do curl -fsS -X DELETE -H \"Authorization: Bearer $CONTAINER_API_KEY\" ",
                "\"https://console.vast.ai/api/v0/instances/$CONTAINER_ID/\" && break; sleep 60; done) ",
                ">/dev/null 2>&1 &"
            )
        );
    }

    #[test]
    fn list_keeps_only_hopbox_labels() {
        let v = Vast {
            cli: FakeCli::with(&[(0, &fixture("instances.json"))]),
        };
        let names: Vec<_> = v
            .list()
            .unwrap()
            .iter()
            .map(|i| i.hopbox_name().unwrap().to_owned())
            .collect();
        assert_eq!(names, ["gpu1", "stray"]);
    }

    #[test]
    fn ssh_url_parse() {
        assert_eq!(
            parse_ssh_url("ssh://root@1.2.3.4:40022"),
            Some(("root".into(), "1.2.3.4".into(), 40022))
        );
        assert_eq!(parse_ssh_url("root@1.2.3.4:22"), None);
        assert_eq!(parse_ssh_url("ssh://root@host"), None);
    }

    #[test]
    fn failed_call_reports_stderr() {
        let v = Vast {
            cli: FakeCli {
                replies: RefCell::new(VecDeque::from([CliOutput {
                    code: 1,
                    stdout: String::new(),
                    stderr: "failed with error 401: unauthorized".into(),
                }])),
                calls: RefCell::default(),
            },
        };
        let e = v.destroy(1).unwrap_err().to_string();
        assert_eq!(
            e,
            "vastai destroy instance failed: failed with error 401: unauthorized"
        );
    }
}
