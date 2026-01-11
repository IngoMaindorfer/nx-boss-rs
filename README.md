# nx-boss

Compatible server for PaperStream NX Manager.

This allows compatible scanners (e.g. fi-7300NX) to scan directly over network, with NO driver, NO Windows, NO USB, NO proprietary software.

## Usage

```console
$ pip install git+https://github.com/SEIAROTg/nix-boss.git
$ curl -OL https://github.com/SEIAROTg/nx-boss/raw/refs/heads/main/config.example.yaml
$ mkdir out  # `output_path` in config
$ python -m nx_boss -c config.example.yaml --host 0.0.0.0 --port 10447
```

Connect your scanner to server and happy scanning!

## Note

- ⚠️ Use at your own risk. The authors are not responsible for any damage.
- Check [code](./src/nx_boss/__main__.py) for a complete list of settings.
- This is not subject to the 400dpi limit in PaperStream NX Manager.
- This is not designed for enterprise use and thus does not include features like authentication, per-scanner job group, log, email, s3, etc., but they should be trivial to implement.
