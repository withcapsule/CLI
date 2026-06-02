use std::{
	process,
	error::{
		Error
	},
	io::{
		ErrorKind
	},
	path::{
		Path,
		PathBuf
	},
	time::{
		SystemTime,
		UNIX_EPOCH
	},
	env::{
		temp_dir
	}
};

use clap::{
	Parser,
	Subcommand
};

use futures_util::{
	StreamExt
};

use indicatif::{
	ProgressBar,
	ProgressStyle
};

use inquire::{
	Select
};

use qrcode::{
	QrCode
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
		File,
		metadata
	},
	io::{
		AsyncWriteExt,
		BufReader
	}
};

use tokio_util::{
	io::{
		ReaderStream
	}
};

use bip39::{
	Mnemonic,
	Language::{
		English
	}
};

use age::{
	Encryptor,
	Decryptor,
	secrecy::{
		SecretString
	}
};

#[derive(Parser)]
#[command(name = "filemover", version, about = "CLI for the FileMover server")]
struct CLI {
	// #[arg(long, default_value = "http://localhost:9001")]
	#[arg(long, default_value = "https://filemover.byseansingh.com")]
	server: String,

	#[command(subcommand)]
	command: Command,
}


#[derive(Subcommand)]
enum Command {
	#[command(visible_alias = "p", about = "Test server connection with a ping")]
	Ping,

	#[command(visible_alias = "u", about = "Upload a file to the server")]
	Upload {
		path: PathBuf,
	},

	#[command(visible_alias = "ue", about = "Locally encrypt a file, then upload a file to the server")]
	UploadEncrypted {
		path: PathBuf,
	},

	#[command(visible_alias = "d", about = "Download a recently uploaded file")]
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
			upload_file( &client, base, path, false ).await?;
		}

		Command::UploadEncrypted { path } => {
			upload_file( &client, base, path, true ).await?;
		}

		Command::Download { id_or_url, output } => {
			download_file( &client, base, id_or_url, output ).await?;
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

fn highlight_path( path: &Path ) -> String {
	format!( "\x1b[94m{}\x1b[0m", path.display() )
}

fn highlight_link( value: &str ) -> String {
	format!( "\x1b[92m{}\x1b[0m", value )
}

fn highlight_id( value: &str ) -> String {
	format!( "\x1b[92m{}\x1b[0m", value )
}

fn encrypt_into_temp_file( path: &Path, passphrase: SecretString ) -> Result<PathBuf, Box<dyn Error>> {
	let mut temp_path: PathBuf = temp_dir();
	temp_path.push(
		format!( "flmvr-{}-{}",
			SystemTime::now().duration_since( UNIX_EPOCH )?.as_nanos(),
			process::id(),
		)
	);

	let input_file = std::fs::File::open( path )?;
	let output_file = std::fs::File::create( temp_path.clone() )?;

	let encryptor: Encryptor = Encryptor::with_user_passphrase( passphrase );
	let mut file_writer = encryptor.wrap_output( output_file )?;
	let mut file_reader = std::io::BufReader::new( input_file );

	std::io::copy( &mut file_reader, &mut file_writer )?;
	file_writer.finish()?;

	return Ok( temp_path );
}

async fn upload_file( client:&Client, base: &str, path: PathBuf, encrypt: bool ) -> Result<(), Box<dyn Error>> {
	let url = if encrypt {
		format!( "{}/curlup?encrypted=true", base )
	} else {
		format!( "{}/curlup", base )
	};

	let metadata = match metadata( &path ).await {
		Ok( metadata ) => metadata,

		Err( err ) if err.kind() == ErrorKind::NotFound => {
			eprintln!( "\nUpload aborted: file {} not found\n", highlight_path( &path ) );
			return Ok( () );
		}

		Err( err ) => return Err( err.into() ),
	};

	if metadata.is_dir() {
		eprintln!( "\nUpload aborted: {} is a directory. Please provide a file path.\n", highlight_path( &path ) );
		return Ok( () );
	}

	let file_size: u64 = metadata.len();

	let pb = ProgressBar::new( file_size );
	pb.set_style( ProgressStyle::default_bar()
		.template( "{spinner:.green} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})" )?
		.progress_chars( "#>-" )
	);

	let mut upload_source_file: Option<PathBuf> = None;
	let mut upload_temp_file: Option<PathBuf> = None;
	let mut upload_decryption_key: Option<String> = None;

	let mnemonic: Mnemonic;
	let mnemonic_text: String;
	let passphrase: SecretString;
	let encrypted_file_path: PathBuf;

	if encrypt {
		mnemonic = Mnemonic::generate_in( English, 12 )?;
		mnemonic_text = mnemonic.to_string();

		passphrase = SecretString::new( mnemonic_text.clone().into() );
		encrypted_file_path = encrypt_into_temp_file( &path, passphrase )?;

		upload_source_file = Some( encrypted_file_path.clone() );
		upload_temp_file = Some( encrypted_file_path );
		upload_decryption_key = Some( mnemonic_text );
	}


	let upload_path = upload_source_file.as_deref().unwrap_or( &path );

	let upload_size = if upload_source_file.is_some() {
		tokio::fs::metadata( upload_path ).await?.len()
	} else {
		file_size
	};

	pb.set_length( upload_size );

	let file = File::open( upload_path ).await?;
	let file_name = path.file_name()
		.map( |n| n.to_string_lossy().to_string() )
		.unwrap_or_else( || "file".to_string() );

	let pb_clone = pb.clone();
	let stream = ReaderStream::new( file ).map( move |chunk| {
		chunk.map( |bytes| {
			pb_clone.inc( bytes.len() as u64 );
			bytes
		} )
	} );

	let body = reqwest::Body::wrap_stream( stream );
	let part = Part::stream_with_length( body, upload_size ).file_name( file_name );
	let form = Form::new().part( "f", part );

	let resp = client.post( url ).multipart( form ).send().await?;
	pb.finish_with_message( "Upload complete" );

	if let Some( temp ) = upload_temp_file {
		let _ = tokio::fs::remove_file( temp ).await;
	}

	let body = resp.text().await?;

	if let Some( file_id ) = extract_file_id( &body ) {
		let download_url = format!( "{}/download/{}", base, file_id );
		println!( "\n\nDownload Link: {}", highlight_link( &download_url ) );
		println!( "File ID: {}\n", highlight_id( &file_id ) );

		let options = if encrypt {
			vec![ "(I) Exit", "(II) Show decryption phrases", "(III) Show download link as QR code", "(IV) Both II and III" ]
		} else {
			vec![ "Exit", "Show download link as QR code" ]
		};

		let selection = Select::new( "What would you like to do?", options ).with_vim_mode( true ).prompt();

		match selection {
			Ok( choice ) if choice == "Show download link as QR code" => {
				println!( "QR Code:" );
				if let Ok( code ) = QrCode::new( &download_url ) {
					let image = code.render::<char>()
						.quiet_zone( true )
						.module_dimensions( 2, 1 )
						.build();
					println!( "{}\n", image );
				}
			}
			Ok( choice ) if choice == "Show decryption phrases" => {
				if let Some( key ) = upload_decryption_key {
					println!( "Decryption Phrases: {}\n", key );
				}
			}
			Ok( choice ) if choice == "Both 2 and 3" => {
				println!( "QR Code:" );
				if let Ok( code ) = QrCode::new( &download_url ) {
					let image = code.render::<char>()
						.quiet_zone( true )
						.module_dimensions( 2, 1 )
						.build();
					println!( "{}\n", image );
				}
				if let Some( key ) = upload_decryption_key {
					println!( "Decryption Phrases: {}\n", key );
				}
			}
			_ => {}
		}
	} else { println!( "\n{}", body.trim_end() ); }

	return Ok( () );
}

async fn download_file( client:&Client, base: &str, id_or_url: String, output: Option<PathBuf> ) -> Result<(), Box<dyn Error>> {
	let id = extract_id_from_input( &id_or_url );
	let url = format!( "{}/download/{}", base, id );
	let resp = client.get( url ).send().await?;

	if !resp.status().is_success() {
		let status = resp.status();
		let body = resp.text().await.unwrap_or_default();

		return Err( format!( "Download failed ({}): {}", status, body ).into() );
	}

	let total_size = resp.content_length().unwrap_or( 0 );
	let pb = ProgressBar::new( total_size );
	pb.set_style( ProgressStyle::default_bar()
		.template( "{spinner:.green} [{wide_bar:.cyan/blue}] {bytes}/{total_bytes} ({eta})" )?
		.progress_chars( "#>-" )
	);

	let filename = output.unwrap_or_else( || {
		filename_from_headers( resp.headers() ).unwrap_or_else( || PathBuf::from( "download" ) )
	} );

	match tokio::fs::metadata( &filename ).await {
		Ok( _ ) => {
			let options = vec![ "No", "Yes" ];
			let selection = Select::new( &format!( "File `{}` already exists. Overwrite?", {filename.to_string_lossy().to_string()} ).to_string(), options ).with_vim_mode( true ).prompt();

			match selection {
				Ok( choice ) if choice == "Yes" => {}
				_ => {
					println!( "\nDownload cancelled.\n" );
					return Ok( () );
				}
			}
		}
		Err( err ) if err.kind() == ErrorKind::NotFound => {}
		Err( err ) => return Err( err.into() ),
	}

	let mut file = File::create( &filename ).await?;
	let mut stream = resp.bytes_stream();

	while let Some( chunk ) = stream.next().await {
		let bytes = chunk?;
		pb.inc( bytes.len() as u64 );
		file.write_all( &bytes ).await?;
	}

	pb.finish_with_message( "Download complete" );
	println!( "\nSaved to: {}\n", filename.display() );

	return Ok( () )
}
