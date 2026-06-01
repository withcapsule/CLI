use std::{
	error::{
		Error
	},
	path::{
		Path,
		PathBuf
	},
};

use clap::{
	Parser,
	Subcommand
};

use futures_util::{
	StreamExt
};

use reqwest::{
	header,
	multipart::{
		Form,
		Part
	},
	Client
};

use tokio::{
	fs::{
		File
	},
	io::{
		AsyncWriteExt
	}
};

#[derive(Parser)]
#[command(name = "filemover", version, about = "CLI for the FileMover server")]
struct CLI {
	#[arg(long, default_value = "http://localhost:9001")]
	server: String,

	#[command(subcommand)]
	command: Command,
}


#[derive(Subcommand)]
enum Command {
	Ping,
	Upload {
		path: PathBuf,
	},
	Download {
		id_or_url: String,

		#[arg(short, long)]
		output: Option<PathBuf>,
	},
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
	let cli = CLI::parse();
	let base = &cli.server.trim_end_matches( '/' ).to_string();
	let client = Client::new();

	match cli.command {
		Command::Ping => {
			let url = format!( "{}/ping", base );
			let resp = client.get( url ).send().await?;
			println!( "{}", resp.text().await? );
		}

		Command::Upload { path } => {
			let url = format!( "{}/curlup", base );

			let part = Part::file( &path ).await?;
			let form = Form::new().part( "f", part );

			let resp = client.post( url ).multipart( form ).send().await?;
			let body = resp.text().await?;

			println!( "{}", body.trim_end() );

			if let Some( file_id ) = extract_file_id( &body ) {
				println!( "Download: {}/download/{}", base, file_id );
			}
		}

		Command::Download { id_or_url, output } => {
			let id = extract_id_from_input( &id_or_url );
			let url = format!( "{}/download/{}", base, id );
			let resp = client.get( url ).send().await?;

			if !resp.status().is_success() {
				let status = resp.status();
				let body = resp.text().await.unwrap_or_default();

				return Err( format!( "Download failed ({}): {}", status, body ).into() );
			}

			let filename = output.unwrap_or_else( || {
				filename_from_headers( resp.headers() ).unwrap_or_else( || PathBuf::from( "download" ) )
			} );

			let mut file = File::create( &filename ).await?;
			let mut stream = resp.bytes_stream();

			while let Some( chunk ) = stream.next().await {
				let bytes = chunk?;
				file.write_all( &bytes ).await?;
			}

			println!( "Saved to {}", filename.display() );
		}
	}

	return Ok( () );
}

fn extract_file_id( body: &str ) -> Option<String> {
	let marker = "File ID for downloading is ";
	let start = body.find( marker )?;
	let after = &body[ start + marker.len().. ];

	let id: String = after.chars().take_while( |c| c.is_ascii_alphanumeric() ).collect();

	if id.is_empty() { None } else { Some( id ) }
}

fn extract_id_from_input( input: &str ) -> String {
	if let Some( idx ) = input.rfind( "/download/" ) {
		let after = &input[ idx + "/download/".len().. ];
		let trimmed = after.trim_matches( '/' );

		if trimmed.is_empty() { input.to_string() } else { trimmed.to_string() }
	} else {
		input.to_string()
	}
}

fn filename_from_headers( headers: &header::HeaderMap ) -> Option<PathBuf> {
	let value = headers.get( header::CONTENT_DISPOSITION )?.to_str().ok()?;
	let filename = parse_filename_param( value )?;
	if filename.is_empty() { None } else { Some( Path::new( &filename ).file_name()?.into() ) }
}

fn parse_filename_param( header_value: &str ) -> Option<String> {
	for part in header_value.split( ';' ) {
		let trimmed = part.trim();
		if let Some( rest ) = trimmed.strip_prefix( "filename=" ) {
			return Some( rest.trim_matches( '"' ).to_string() );
		}
		if let Some( rest ) = trimmed.strip_prefix( "filename*=" ) {
			let decoded = rest.trim_matches( '"' );
			if let Some( idx ) = decoded.find( "''" ) {
				return Some( decoded[ idx + 2.. ].to_string() );
			}
			return Some( decoded.to_string() );
		}
	}

	return None;
}
