//! external rebind through the public embedding API.
use moonseed::*;
use std::rc::Rc;
mod support;
use support::*;
struct Assets(Rc<String>);
struct Asset {
    key: Vec<u8>,
    data: Rc<String>,
}
impl HostUserdata for Asset {
    const SYMBOL: &'static str = "example.Asset";
    fn logical_size(&self) -> u64 {
        self.key.len() as u64
    }
}
impl RebindUserdata for Asset {
    fn key(&self) -> Vec<u8> {
        self.key.clone()
    }
    fn rebind(key: &[u8], env: &HostEnv) -> std::result::Result<Self, RebindError> {
        if key != b"game/answer" {
            return Err(RebindError("unknown asset key"));
        }
        let assets = env.get::<Assets>().ok_or(RebindError("assets missing"))?;
        Ok(Self {
            key: key.to_vec(),
            data: assets.0.clone(),
        })
    }
}
fn main() -> ExampleResult {
    let mut registry = HostRegistry::new();
    registry.register_rebind_userdata::<Asset>();
    let mut rt = Runtime::builder().registry(registry.clone()).build()?;
    let object = rt.create_host_userdata(
        Asset {
            key: b"game/answer".to_vec(),
            data: Rc::new("old host resource".into()),
        },
        0,
    )?;
    rt.globals().raw_set(&mut rt, "asset", object)?;
    let snapshot = rt.snapshot()?;
    let data: Rc<String> = Rc::new("restored host resource".into());
    let mut env = HostEnv::new();
    env.insert(Assets(data.clone()));
    let mut restored = Runtime::restore(&snapshot, &Host::new(registry).host_env(env))?;
    let asset: AnyUserData = restored.globals().raw_get(&mut restored, "asset")?;
    let asset = asset.borrow::<Asset>(&restored)?;
    assert_eq!(asset.data.as_str(), "restored host resource");
    assert!(Rc::ptr_eq(&asset.data, &data));
    Ok(())
}

#[cfg(test)]
#[test]
fn example_runs_and_asserts_its_result() {
    main().unwrap();
}
