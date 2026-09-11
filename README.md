<p align="center">
  <img src="duckxy-logo.svg" alt="duckxy" width="240">
</p>

# duckxy

On-the-fly URL-based geospatial processing using DuckDB

## What is duckxy

**duckxy** /ˈdʌk.siː/ *duck-see*

The duckxy project is inspired by [imgproxy](https://imgproxy.net), which put image processing behind a URL.
Instead of resizing a file and uploading the result, you ask for the size you want and the server does the rest.
duckxy does the same for geospatial data, so a filtered or reprojected layer is a URL rather than a preprocessing job.

duckxy uses [DuckDB](https://duckdb.org) and its [spatial extension](https://duckdb.org/docs/stable/core_extensions/spatial/overview) to process geospatial data.

With this, the project is named duckxy: **duck** from DuckDB, and **xy** from the end of *proxy*, which is also the X and Y of a coordinate pair, the thing geospatial data is made of.
