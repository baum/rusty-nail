local_resource('compile', 'just compile')
docker_build('quay.io/baum/rusty-nail', '.', dockerfile='Dockerfile')
k8s_yaml('yaml/deployment.yaml')
k8s_resource('noobaa-source-controller', port_forwards=8080)
