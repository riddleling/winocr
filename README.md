# winocr

An OCR Tool using Windows.Media.Ocr.OcrEngine API

## Command Line Arguments

```
An OCR Tool using Windows.Media.Ocr.OcrEngine API.

Usage: winocr.exe [OPTIONS] [FILES]...

Arguments:
  [FILES]...  Input files

Options:
  -o, --ocr          OCR and export text files
  -s, --server       Run HTTP Server
  -a, --auth <AUTH>  HTTP Basic Auth (username:password) [default: ]
  -p, --port <PORT>  HTTP port number [default: 8000]
  -h, --help         Print help
  -V, --version      Print version
```

## How to use

### Read images and perform OCR, then output the result to text files

```
winocr -o *.png
```

### Start the OCR HTTP server and specify the HTTP port

```
winocr -s -p 8080
```

### Start the OCR HTTP server and configure HTTP Basic Auth

```
macocr -s -a admin:password123 -p 8080
```

After starting the HTTP server, you can upload an image from the homepage HTML or use `curl` to send an image via the `upload` API

```
curl -u admin:password123 -H "Accept: application/json" -X POST http://localhost:8080/upload -F "file=@01.png"
```


## License

MIT License


