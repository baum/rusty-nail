use kube::CustomResourceExt;
fn main() {
    print!("{}", serde_yaml::to_string(&controller::NooBaaSource::crd()).unwrap())
}
