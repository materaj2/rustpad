pub mod encode;
pub mod forged_cypher_text;

use crate::block::{block_size::BlockSizeTrait, Block};
use std::borrow::Cow;

use anyhow::{anyhow, Context, Result};

use crate::block::block_size::BlockSize;

use self::encode::{AmountBlocksTrait, Encode, Encoding};

#[derive(Debug, Clone)]
pub struct CypherText {
    blocks: Vec<Block>,
    url_encoded: bool,
    used_encoding: Encoding,
}

impl CypherText {
    pub fn parse(
        input_data: &str,
        block_size: &BlockSize,
        no_iv: bool,
        specified_encoding: Option<Encoding>,
        no_url_encode: bool,
    ) -> Result<Self> {
        let url_decoded = if no_url_encode {
            Cow::Borrowed(input_data)
        } else {
            // detect url encoding automatically and decode if needed
            urlencoding::decode(input_data).unwrap_or(Cow::Borrowed(input_data))
        };

        let (decoded_data, used_encoding) = decode(&url_decoded, specified_encoding)?;
        let blocks = split_into_blocks(&decoded_data[..], *block_size)?;
        let blocks = if no_iv {
            [Block::new(block_size)]
                .into_iter()
                .chain(blocks.into_iter())
                .collect()
        } else {
            blocks
        };

        if blocks.len() == 1 {
            return Err(anyhow!("Decryption impossible with only 1 block. Does this cypher text include an IV? If not, indicate that with `--no-iv`"));
        }

        Ok(Self {
            blocks,
            url_encoded: input_data != url_decoded,
            used_encoding,
        })
    }

    pub fn from_iter<'a>(
        blocks: impl IntoIterator<Item = &'a Block>,
        url_encoded: bool,
        used_encoding: Encoding,
    ) -> Self {
        Self {
            blocks: blocks.into_iter().cloned().collect(),
            url_encoded,
            used_encoding,
        }
    }
}

impl<'a> Encode<'a> for CypherText {
    type Blocks = &'a [Block];

    fn encode(&'a self) -> String {
        let raw_bytes: Vec<u8> = self
            .blocks()
            .iter()
            .map(|block| &**block)
            .flatten()
            // blocks are scattered through memory, gotta collect them
            .cloned()
            .collect();

        let encoded_data = match self.used_encoding() {
            Encoding::Hex => hex::encode(raw_bytes),
            Encoding::Base64 => base64::encode_config(raw_bytes, base64::STANDARD),
            Encoding::Base64Url => base64::encode_config(raw_bytes, base64::URL_SAFE),
        };

        if *self.url_encoded() {
            urlencoding::encode(&encoded_data).to_string()
        } else {
            encoded_data
        }
    }

    fn blocks(&'a self) -> Self::Blocks {
        &self.blocks[..]
    }

    fn url_encoded(&self) -> &bool {
        &self.url_encoded
    }

    fn used_encoding(&self) -> &Encoding {
        &self.used_encoding
    }
}

impl BlockSizeTrait for CypherText {
    fn block_size(&self) -> BlockSize {
        self.blocks()[0].block_size()
    }
}

impl AmountBlocksTrait for CypherText {
    fn amount_blocks(&self) -> usize {
        self.blocks().len()
    }
}

fn decode(input_data: &str, specified_encoding: Option<Encoding>) -> Result<(Vec<u8>, Encoding)> {
    fn auto_decode(input_data: &str) -> Result<(Vec<u8>, Encoding)> {
        if let Ok(decoded_data) = hex::decode(&*input_data) {
            return Ok((decoded_data, Encoding::Hex));
        }

        if let Ok(decoded_data) = base64::decode_config(&*input_data, base64::STANDARD) {
            return Ok((decoded_data, Encoding::Base64));
        }

        if let Ok(decoded_data) = base64::decode_config(&*input_data, base64::URL_SAFE) {
            return Ok((decoded_data, Encoding::Base64Url));
        }

        Err(anyhow!(
            "`{}` has an invalid or unsupported encoding",
            input_data
        ))
    }

    fn forced_decode(
        input_data: &str,
        specified_encoding: Encoding,
    ) -> Result<(Vec<u8>, Encoding)> {
        let decoded_data = match specified_encoding {
            Encoding::Hex => {
                hex::decode(&*input_data).context(format!("`{}` is not valid hex", input_data))
            }
            Encoding::Base64 => base64::decode_config(&*input_data, base64::STANDARD)
                .context(format!("`{}` is not valid base64", input_data)),
            Encoding::Base64Url => base64::decode_config(&*input_data, base64::URL_SAFE)
                .context(format!("`{}` is not valid base64 (URL safe)", input_data)),
        }
        .context("Invalid encoding for cypher text specified")?;

        Ok((decoded_data, specified_encoding))
    }

    if let Some(specified_encoding) = specified_encoding {
        forced_decode(input_data, specified_encoding)
    } else {
        auto_decode(input_data)
    }
}

fn split_into_blocks(decoded_data: &[u8], block_size: BlockSize) -> Result<Vec<Block>> {
    if decoded_data.len() % (*block_size as usize) != 0 {
        return Err(anyhow!(
            "Splitting cypher text into blocks of {} bytes failed. Double check the block size",
            *block_size
        ));
    }

    let blocks = decoded_data
        .chunks_exact(*block_size as usize)
        .map(|chunk| chunk.into())
        .collect();

    Ok(blocks)
}
